//! `kernelctl timeout` - read or write the boot menu timeout.

use crate::error::{Error, Result};
use crate::loaders::{Capabilities, Timeout};
use crate::ui::style;

use super::{dry_run_notice, success, App};

pub fn run(app: &App, value: Option<&str>) -> Result<()> {
    let loader = app.loader()?;

    match value {
        None => show(app),
        Some(v) => {
            if !loader.capabilities().contains(Capabilities::TIMEOUT) {
                return Err(Error::unsupported(
                    loader.kind().display_name(),
                    "menu timeout configuration",
                ));
            }
            let timeout = Timeout::parse(v)?;

            if app.args.dry_run {
                dry_run_notice(&format!("set the boot menu timeout to {timeout}"));
                return Ok(());
            }

            let outcomes = loader.set_timeout(&app.context(), timeout)?;
            success(&format!("boot menu timeout is now {}", style::bold(&timeout.to_string())));

            // A hidden menu is a real trap: with no timeout there is no
            // opportunity to pick a different entry if the default fails.
            if timeout == Timeout::Immediate {
                super::note_line(
                    "the menu will not be shown; most loaders still let you hold a key \
                     during boot to reach it",
                );
            }

            app.report_writes(&outcomes);
            app.print_note(loader);
            Ok(())
        }
    }
}

fn show(app: &App) -> Result<()> {
    let loader = app.loader()?;
    let current = loader.timeout(&app.context())?;

    if app.args.json {
        #[derive(serde::Serialize)]
        struct Out {
            loader: String,
            timeout: Option<String>,
            seconds: Option<u32>,
            configurable: bool,
        }
        return super::print_json(&Out {
            loader: loader.kind().to_string(),
            timeout: current.map(|t| t.to_string()),
            seconds: match current {
                Some(Timeout::Seconds(n)) => Some(n),
                Some(Timeout::Immediate) => Some(0),
                _ => None,
            },
            configurable: loader.capabilities().contains(Capabilities::TIMEOUT),
        });
    }

    match current {
        Some(t) => println!("{t}"),
        None => println!(
            "{}",
            style::dim(&format!(
                "{} does not report a timeout; it is using its built-in default",
                loader.kind().display_name()
            ))
        ),
    }
    Ok(())
}
