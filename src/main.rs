#![feature(rustc_private)]
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_infer;
extern crate rustc_interface;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_next_trait_solver;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_trait_selection;
extern crate rustc_type_ir;

use std::sync::OnceLock;

use rustc_data_structures::fx::{FxHashMap, FxHashSet, FxIndexSet};
use rustc_driver::{
    Callbacks, Compilation, DEFAULT_BUG_REPORT_URL, args, catch_with_exit_code,
    init_rustc_env_logger, install_ctrlc_handler, install_ice_hook, run_compiler,
};
use rustc_hir as hir;
use rustc_hir::HirId;
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
use rustc_infer::infer::{InferCtxt, TyCtxtInferExt};
use rustc_interface::Config;
use rustc_interface::interface::Compiler;
use rustc_lint::{Level, Lint};
use rustc_middle::middle::privacy;
use rustc_middle::mir::mono::MonoItem;
use rustc_middle::mir::visit::Visitor as _;
use rustc_middle::mir::{self, CastKind, Rvalue};
use rustc_middle::ty::adjustment::PointerCoercion;
use rustc_middle::ty::{ParamEnv, TraitRef, TyCtxt, TypingEnv};
use rustc_session::EarlyDiagCtxt;
use rustc_session::config::ErrorOutputType;
use rustc_span::{DUMMY_SP, Ident, Span, Symbol};
use rustc_trait_selection::solve::inspect::{InspectGoal, ProofTreeInferCtxtExt, ProofTreeVisitor};
use rustc_type_ir::solve::inspect::ProbeKind;
use rustc_type_ir::solve::{CandidateSource, Goal};

struct Visitor<'a, 'tcx> {
    span: Span,
    infcx: &'a InferCtxt<'tcx>,
    param_env: ParamEnv<'tcx>,
    traits: &'a mut FxHashMap<DefId, FxHashSet<DefId>>,
}

struct MirVisitor<'a, 'tcx> {
    body: &'a mir::Body<'tcx>,
    v: &'a mut Visitor<'a, 'tcx>,
}

impl<'a, 'tcx> Visitor<'a, 'tcx> {
    fn mark_impl(&mut self, i: DefId) {
        if !i.is_local() {
            return;
        }
        let Some(tr) = self.infcx.tcx.trait_id_of_impl(i) else {
            return;
        };
        let Some(set) = self.traits.get_mut(&tr) else {
            return;
        };
        set.insert(i);
    }
    fn visit_mir(&'a mut self, body: &'a mir::Body<'tcx>) {
        MirVisitor { body, v: self }.visit_body(body);
    }
}

impl<'tcx> mir::visit::Visitor<'tcx> for MirVisitor<'_, 'tcx> {
    fn visit_const_operand(&mut self, constant: &mir::ConstOperand<'tcx>, location: mir::Location) {
        self.super_const_operand(constant, location);

        let mir::Const::Unevaluated(uc, _) = &constant.const_ else {
            return;
        };
        let Some(did) = self.v.infcx.tcx.trait_of_assoc(uc.def) else {
            return;
        };
        let tr = TraitRef::from_method(self.v.infcx.tcx, did, uc.args);
        self.v.infcx.visit_proof_tree(
            Goal::new(self.v.infcx.tcx, self.v.param_env, tr),
            &mut *self.v,
        );
    }
    fn visit_rvalue(&mut self, rvalue: &Rvalue<'tcx>, location: mir::Location) {
        self.super_rvalue(rvalue, location);
        let tcx = self.v.infcx.tcx;
        // unsize predicates that can have dyn traits which counts as usage
        let Rvalue::Cast(CastKind::PointerCoercion(PointerCoercion::Unsize, _), op, ty) = rvalue
        else {
            return;
        };
        let lhs_ty = op.ty(&self.body.local_decls, tcx);
        let coerce = tcx.require_lang_item(hir::LangItem::CoerceUnsized, DUMMY_SP);
        let tr = TraitRef::new(tcx, coerce, [lhs_ty, *ty]);
        self.v
            .infcx
            .visit_proof_tree(Goal::new(tcx, self.v.param_env, tr), &mut *self.v);
    }
}

impl<'tcx> ProofTreeVisitor<'tcx> for Visitor<'_, 'tcx> {
    fn span(&self) -> Span {
        self.span
    }
    fn visit_goal(&mut self, goal: &InspectGoal<'_, 'tcx>) {
        for cand in goal.candidates() {
            if cand.result().is_err() {
                continue;
            }
            cand.visit_nested_no_probe(self);

            let ProbeKind::TraitCandidate {
                source: CandidateSource::Impl(i),
                ..
            } = cand.kind()
            else {
                return;
            };
            self.mark_impl(i);
        }
    }
}

struct RedetectCallbacks;

static LINT: Lint = Lint {
    name: "redetect::unused",
    default_level: Level::Warn,
    desc: "unused",
    edition_lint_opts: None,
    report_in_external_macro: true,
    future_incompatible: None,
    is_externally_loaded: true,
    feature_gate: None,
    crate_level_only: false,
    eval_always: true,
};

impl Callbacks for RedetectCallbacks {
    fn config(&mut self, c: &mut Config) {
        c.register_lints = Some(Box::new(|_, store| {
            store.register_lints(&[&LINT]);
        }));
        c.override_queries = Some(|_, prov| {
            static TOOLS: OnceLock<fn(TyCtxt<'_>, ()) -> FxIndexSet<Ident>> = OnceLock::new();
            TOOLS.set(prov.queries.registered_tools).unwrap();
            prov.queries.registered_tools = |tcx, ()| {
                let mut tools = TOOLS.get().unwrap()(tcx, ());
                tools.insert(Ident::new(Symbol::intern("redetect"), DUMMY_SP));
                tools
            };
        })
    }
    fn after_analysis<'tcx>(&mut self, _compiler: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
        /* if tcx.crate_name(LOCAL_CRATE).as_str() != "rustc_metadata" {
            return Compilation::Continue;
        } */

        let effective_vis = tcx.effective_visibilities(());
        let mut traits: FxHashMap<DefId, FxHashSet<DefId>> = tcx
            .traits(LOCAL_CRATE)
            .iter()
            .copied()
            .filter(|&tr| {
                // traits that are never reachable to foreign crates
                !effective_vis.is_public_at_level(
                    tr.as_local().unwrap(),
                    privacy::Level::ReachableThroughImplTrait,
                )
            })
            .map(|did| (did, FxHashSet::default()))
            .collect();

        let mono_items = tcx.collect_and_partition_mono_items(());
        for cgu in mono_items.codegen_units {
            for (item, _) in cgu.items_in_deterministic_order(tcx) {
                // println!("{item:?}");
                if let MonoItem::Fn(f) = item {
                    let env = TypingEnv::post_analysis(tcx, f.def.def_id());
                    let (infcx, param_env) = tcx
                        .infer_ctxt()
                        .with_next_trait_solver(true)
                        .build_with_typing_env(env);
                    let mut visitor = Visitor {
                        span: DUMMY_SP,
                        infcx: &infcx,
                        param_env,
                        traits: &mut traits,
                    };
                    if let Some(impl_) = tcx.impl_of_assoc(f.def_id()) {
                        visitor.mark_impl(impl_);
                    }
                    let preds = tcx.predicates_of(f.def.def_id());
                    for (pred, span) in preds.instantiate(tcx, f.args) {
                        visitor.span = span;
                        infcx.visit_proof_tree(Goal::new(tcx, param_env, pred), &mut visitor);
                    }
                    visitor.visit_mir(tcx.instance_mir(f.def));
                }
            }
        }

        for (tr, set) in traits {
            for imp in tcx.local_trait_impls(tr) {
                if set.contains(&imp.to_def_id()) {
                    continue;
                }
                let ty = tcx.type_of(*imp).instantiate_identity();
                let tr = tcx.item_name(tr);
                let span = tcx.sess.source_map().guess_head_span(tcx.def_span(*imp));
                tcx.node_span_lint(&LINT, HirId::make_owner(*imp), span, |diag| {
                    diag.primary_message(format!("implementation of {tr} for {ty} is unused"));
                });
            }
        }

        Compilation::Continue
    }
}

fn main() -> ! {
    let early_dcx = EarlyDiagCtxt::new(ErrorOutputType::default());

    init_rustc_env_logger(&early_dcx);
    let mut callbacks = RedetectCallbacks;
    install_ice_hook(DEFAULT_BUG_REPORT_URL, |_| ());
    install_ctrlc_handler();
    let mut args = args::raw_args(&early_dcx);
    if args.get(1).is_some_and(|s| s.contains("rustc")) {
        args.remove(1);
    }

    let exit_code = catch_with_exit_code(|| run_compiler(&args, &mut callbacks));

    std::process::exit(exit_code)
}
