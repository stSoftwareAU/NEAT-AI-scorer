//! Opt-in cached-halving experiment; the ordinary scorer CLI is unchanged.
fn main() -> std::process::ExitCode {
    rust_scorer::halving_experiment::main()
}
