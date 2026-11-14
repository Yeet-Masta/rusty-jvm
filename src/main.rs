mod cli;
use crate::cli::argument_parser::into_args;
use crate::cli::help::help_msg;
use clap::{Arg, ArgAction, Command};
use rusty_jvm::{run, Arguments};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let matches = Command::new("rusty-jvm")
        .arg(
            Arg::new("args")
                .action(ArgAction::Append)
                .num_args(1..) // capture everything after program name
                .trailing_var_arg(true)
                .allow_hyphen_values(true),
        )
        .override_help(help_msg())
        .version(env!("CARGO_PKG_VERSION"))
        .get_matches();

    let raw_args = matches
        .get_many::<String>("args")
        .unwrap_or_default()
        .cloned()
        .collect::<Vec<_>>();

    let args = match into_args(raw_args) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    // Like the `java` launcher, run the VM on a thread of its own: the process's main thread has
    // a small native stack (1 MB on Windows), and nested interpreter re-entries need more.
    let vm_thread = std::thread::Builder::new()
        .name("main".to_string())
        .stack_size(rusty_jvm::VM_THREAD_STACK_BYTES)
        .spawn(move || handle_execution(args));
    let outcome = match vm_thread {
        Ok(handle) => handle
            .join()
            .unwrap_or_else(|_| Err("VM thread panicked".to_string())),
        Err(error) => Err(format!("Failed to start the VM thread: {error}")),
    };
    if let Err(msg) = outcome {
        if !msg.is_empty() {
            eprintln!("{msg}");
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn handle_execution(arguments: Arguments) -> Result<(), String> {
    if arguments.entry_point().is_empty() {
        return Err(format!(
            "No entry point provided. Please specify the main class to run.\n{}",
            help_msg()
        ));
    }

    let java_home = PathBuf::from(env::var("JAVA_HOME")
        .map_err(|_| "The JAVA_HOME environment variable is not set.\nSet JAVA_HOME to an existing JDK 25 (or newer) directory.".to_string())?);
    if !java_home.exists() {
        return Err(format!("JAVA_HOME path does not exist: {:?}", java_home));
    }

    run(&arguments, &java_home).map_err(|err| {
        if err.is_uncaught_exception() {
            String::new() // no error message needed for exceptions since it already handled by the VM
        } else {
            format!("VM execution failed: {}", err)
        }
    })
}
