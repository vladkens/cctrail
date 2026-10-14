// Copyright (c) vladkens | MIT License | https://github.com/vladkens/cctrail

mod cli;
mod config;
mod metrics;
mod model;
mod output;
mod parser;
mod pricing;

use std::process::ExitCode;

use cli::Parsed;

fn main() -> ExitCode {
  let argv: Vec<String> = std::env::args().skip(1).collect();
  let res = match cli::parse(&argv) {
    Ok(Parsed::Help) => {
      output::emit(&cli::help());
      Ok(())
    }
    Ok(Parsed::Version) => {
      output::emit(&format!("cctrail {}\n", cli::VERSION));
      Ok(())
    }
    Ok(Parsed::Run(args)) => cli::run(&args),
    Err(e) => Err(e),
  };
  match res {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("cctrail: {e:#}");
      ExitCode::from(2)
    }
  }
}
