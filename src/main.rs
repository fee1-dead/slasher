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

mod unused_private_trait_impls;

use std::sync::OnceLock;

use regex::Regex;
use rustc_data_structures::fx::FxIndexSet;
use rustc_driver::{
    Callbacks, Compilation, DEFAULT_BUG_REPORT_URL, args, catch_with_exit_code,
    init_rustc_env_logger, install_ctrlc_handler, install_ice_hook, run_compiler,
};
use rustc_hir::def_id::LOCAL_CRATE;
use rustc_interface::Config;
use rustc_interface::interface::Compiler;
use rustc_lint::{Level, Lint};
use rustc_middle::ty::TyCtxt;
use rustc_session::EarlyDiagCtxt;
use rustc_session::config::ErrorOutputType;
use rustc_span::{DUMMY_SP, Ident, Symbol};

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
        let current_crate = tcx.crate_name(LOCAL_CRATE);
        /* if current_crate.as_str() != "rustc_metadata" {
            return Compilation::Continue;
        } */
        if let Ok(root) = std::env::var("SLASHER_WORKSPACE_ROOT")
        {
            // workspace reports don't need to care about private trait impls since we work on all traits
            // https://github.com/rust-lang/regex/discussions/737
            let Ok(re) = Regex::new(&format!("^(?:{root})$")) else { return Compilation::Continue };
            if !re.is_match(current_crate.as_str()) {
                return Compilation::Continue;
            }

            

        } else {
            unused_private_trait_impls::run(tcx);
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
