mod cleanup;
mod cli;

use cleanup::{apply_cleanup, print_report, scan};
use cli::{Config, parse_or_exit};

fn main() {
    let config = parse_or_exit();

    if let Err(message) = run(config) {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run(config: Config) -> Result<(), String> {
    let report = scan(&config)?;
    print_report(&config, &report);

    if config.apply {
        let outcome = apply_cleanup(report);
        outcome.print();
    }

    Ok(())
}
