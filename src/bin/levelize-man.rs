//! Writes the levelize(1) man page, generated from the clap definition, to
//! stdout. Built only with the `mangen` feature:
//!
//! ```console
//! $ cargo run --release --features mangen --bin levelize-man > doc/levelize.1
//! ```
//!
//! (or `make man`).

use clap::CommandFactory;
use levelize::cli::Cli;

fn main() -> std::io::Result<()> {
    clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout().lock())
}
