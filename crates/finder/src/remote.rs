use crate::config::RunOn;
use collections::HashMap;
use gpui::Entity;
use remote::{Interactive, RemoteClient};
use std::path::{Path, PathBuf};

/// A command transformed for the project's execution context: a transport
/// launcher (e.g. `ssh`) wrapping the user's command when executed remotely.
#[derive(Debug, PartialEq)]
pub struct ResolvedCommand {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
}

pub(crate) fn resolve_command<C: gpui::AppContext>(
    remote_client: Option<&Entity<RemoteClient>>,
    run_on: RunOn,
    command: String,
    args: Vec<String>,
    cwd: &Path,
    env: HashMap<String, String>,
    cx: &C,
) -> anyhow::Result<ResolvedCommand> {
    match (run_on, remote_client) {
        (_, None) => Ok(ResolvedCommand {
            command,
            args,
            env,
            cwd: Some(cwd.to_path_buf()),
        }),
        (RunOn::Local, Some(_)) => Ok(ResolvedCommand {
            command,
            args,
            env,
            cwd: None,
        }),
        (RunOn::Auto, Some(remote_client)) => {
            let template = remote_client
                .read_with(cx, |client, _| {
                    client.build_command(
                        Some(command),
                        &args,
                        &env,
                        Some(cwd.to_string_lossy().to_string()),
                        None,
                        Interactive::No,
                    )
                })?;
            Ok(ResolvedCommand {
                command: template.program,
                args: template.args,
                env: template.env,
                cwd: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};

    fn local_env() -> HashMap<String, String> {
        HashMap::from_iter([("PATH".to_string(), "/project/.bin".to_string())])
    }

    async fn mock_remote_client(
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) -> Entity<RemoteClient> {
        init_release_channel(cx, server_cx);
        let (opts, server_session, connect_guard) = RemoteClient::fake_server(cx, server_cx);

        let ping_handler = server_cx.new(|_| ());
        server_session.add_request_handler::<rpc::proto::Ping, _, _, _>(
            ping_handler.downgrade(),
            |_entity, _envelope, _cx| async { Ok(rpc::proto::Ack {}) },
        );
        drop(connect_guard);
        RemoteClient::connect_mock(opts, cx).await
    }

    fn init_release_channel(cx: &mut TestAppContext, server_cx: &mut TestAppContext) {
        let version = semver::Version::new(0, 0, 0);
        cx.update(|cx| release_channel::init(version.clone(), cx));
        server_cx.update(|cx| release_channel::init(version, cx));
    }

    #[gpui::test]
    async fn auto_routes_through_the_transport_when_a_remote_client_is_present(
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) {
        let client = mock_remote_client(cx, server_cx).await;

        cx.update(|cx| {
            let resolved = resolve_command(
                Some(&client),
                RunOn::Auto,
                "rg".into(),
                vec!["--files".into()],
                Path::new("/remote/project"),
                local_env(),
                cx,
            )
            .expect("the mock transport builds commands");

            assert_eq!(resolved.command, "mock");
            assert_eq!(resolved.args, vec!["rg", "--files"]);
            assert_eq!(resolved.cwd, None, "the cd belongs to the transport");
            assert_eq!(resolved.env, local_env());
        });
    }

    #[gpui::test]
    async fn local_overrides_the_transport_and_drops_the_remote_working_directory(
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) {
        let client = mock_remote_client(cx, server_cx).await;

        cx.update(|cx| {
            let resolved = resolve_command(
                Some(&client),
                RunOn::Local,
                "rg".into(),
                vec!["--files".into()],
                Path::new("/remote/project"),
                local_env(),
                cx,
            )
            .expect("forced-local never consults the transport");

            assert_eq!(resolved.command, "rg");
            assert_eq!(resolved.args, vec!["--files"]);
            assert_eq!(
                resolved.cwd, None,
                "a remote path must not reach a local current_dir"
            );
        });
    }

    #[gpui::test]
    async fn without_a_remote_client_the_command_runs_locally_at_the_working_directory(
        cx: &mut TestAppContext,
        _server_cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            let resolved = resolve_command(
                None,
                RunOn::Auto,
                "rg".into(),
                vec!["--files".into()],
                Path::new("/local/project"),
                local_env(),
                cx,
            )
            .expect("the local branch cannot fail");

            assert_eq!(resolved.command, "rg");
            assert_eq!(resolved.args, vec!["--files"]);
            assert_eq!(resolved.cwd, Some(PathBuf::from("/local/project")));
        });
    }

    #[gpui::test]
    async fn an_unavailable_transport_connection_is_an_error_not_a_local_fallback(
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) {
        let client = {
            init_release_channel(cx, server_cx);
            let (opts, server_session, connect_guard) =
                RemoteClient::fake_server(cx, server_cx);

            let ping_handler = server_cx.new(|_| ());
            server_session.add_request_handler::<rpc::proto::Ping, _, _, _>(
                ping_handler.downgrade(),
                |_entity, _envelope, _cx| async { Ok(rpc::proto::Ack {}) },
            );
            drop(connect_guard);
            RemoteClient::connect_mock(opts, cx).await
        };

        cx.update(|cx| {
            client.update(cx, |client, cx| client.force_server_not_running(cx));
        });

        cx.update(|cx| {
            let result = resolve_command(
                Some(&client),
                RunOn::Auto,
                "rg".into(),
                vec!["--files".into()],
                Path::new("/remote/project"),
                local_env(),
                cx,
            );

            let error = result.expect_err("no transport connection");
            assert!(
                error.to_string().contains("connection"),
                "unexpected error: {error}"
            );
        });
    }
}
