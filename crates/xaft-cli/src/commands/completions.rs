//! Handler for `xaft completions`.

use clap::CommandFactory;
use clap_complete::generate;
use xaft_runtime::ExitCode;

use crate::args::{CompletionsArgs, ShellArg, XaftCli};
use crate::error::XaftError;

/// Execute `xaft completions <shell>`.
pub async fn handle_completions(args: &CompletionsArgs) -> Result<ExitCode, XaftError> {
    let mut cmd = XaftCli::command();
    let name = cmd.get_name().to_string();

    generate(
        clap_complete::Shell::from(args.shell),
        &mut cmd,
        name,
        &mut std::io::stdout(),
    );

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completions_bash() {
        let args = CompletionsArgs {
            shell: ShellArg::Bash,
        };
        let code = handle_completions(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn completions_zsh() {
        let args = CompletionsArgs {
            shell: ShellArg::Zsh,
        };
        let code = handle_completions(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn completions_fish() {
        let args = CompletionsArgs {
            shell: ShellArg::Fish,
        };
        let code = handle_completions(&args).await.unwrap();
        assert!(code.is_success());
    }
}
