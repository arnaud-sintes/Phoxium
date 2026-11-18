//! ultimatecrt: captures a window, runs it through a (for now passthrough) pipeline and presents
//! it in a separate output window. Run `ultimatecrt --help` for the options.

fn main() {
    if let Err(e) = ultimatecrt::app::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
