use regex::Regex;
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_errors::Diag;
use rustc_hir as hir;
use rustc_hir::HirId;
use rustc_hir::def_id::DefId;
use rustc_infer::infer::{InferCtxt, TyCtxtInferExt};
use rustc_lint::Level;
use rustc_middle::lint::{LevelAndSource, LintLevelSource, lint_level};
use rustc_middle::mir::mono::MonoItem;
use rustc_middle::mir::visit::Visitor as _;
use rustc_middle::mir::{self, CastKind, Rvalue};
use rustc_middle::ty::adjustment::PointerCoercion;
use rustc_middle::ty::{ParamEnv, TraitRef, TyCtxt, TypingEnv};
use rustc_span::{DUMMY_SP, Span};
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

pub(super) fn run(tcx: TyCtxt<'_>) {
    let re = std::env::var("SLASHER_TRAIT_RE").unwrap();
    let re = Regex::new(&format!("^(?:{re})$")).unwrap();
    let mut traits: FxHashMap<DefId, FxHashSet<DefId>> = tcx
        .all_traits_including_private()
        .filter(|tr| re.is_match(tcx.item_name(tr).as_str()))
        .map(|did| (did, FxHashSet::default()))
        .collect();

    let mono_items = tcx.collect_and_partition_mono_items(());
    for cgu in mono_items.codegen_units {
        for (item, _) in cgu.items_in_deterministic_order(tcx) {
            println!("{item:?}");
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

    let wre = std::env::var("SLASHER_WORKSPACE_RE").unwrap();
    let wre = Regex::new(&format!("^(?:{wre})$")).unwrap();
    for (tr, set) in traits {
        for imp in tcx.all_impls(tr) {
            if !wre.is_match(tcx.crate_name(imp.krate).as_str()) {
                continue;
            }
            if set.contains(&imp) {
                continue;
            }
            let ty = tcx.type_of(imp).instantiate_identity();
            let tr = tcx.item_name(tr);
            let span = tcx.sess.source_map().guess_head_span(tcx.def_span(imp));
            let decorate = |diag: &mut Diag<'_, ()>| {
                diag.primary_message(format!("implementation of {tr} for {ty} is unused"));
            };
            if let Some(local) = imp.as_local() {
                tcx.node_span_lint(&crate::LINT, HirId::make_owner(local), span, decorate);
            } else {
                lint_level(
                    tcx.sess,
                    &crate::LINT,
                    LevelAndSource {
                        level: Level::Warn,
                        lint_id: None,
                        src: LintLevelSource::Default,
                    },
                    Some(span.into()),
                    decorate,
                );
            }
        }
    }
}
