use std::future::pending;

use openlogi_core::module::ModuleId;
use openlogi_ipc::testing::in_memory_agent;
use openlogi_ipc::{AgentRequest, AgentResponse, ModuleInfo};

use super::*;

/// The longest a command may take against the in-memory agent.
const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

fn list() -> ModulesArgs {
    ModulesArgs {
        action: ModulesAction::List,
    }
}

async fn run_with(modules: Vec<ModuleStatus>) -> String {
    let client = in_memory_agent(
        move |request| {
            assert!(
                matches!(request, AgentRequest::ListModules {}),
                "{request:?}"
            );
            let modules = modules.clone();
            Box::pin(async move { Ok(AgentResponse::ListModules(modules)) })
        },
        pending(),
    );
    let mut out = Vec::new();
    tokio::time::timeout(WAIT, execute(list(), Ok(client), &mut out))
        .await
        .expect("the command finishes in time")
        .unwrap();
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn an_empty_folder_says_so() {
    assert_eq!(run_with(Vec::new()).await, "No modules installed.\n");
}

#[tokio::test]
async fn each_folder_is_listed_with_its_state_and_error() {
    let out = run_with(vec![
        ModuleStatus {
            folder: "broken".into(),
            module: None,
            enabled: false,
            state: ModuleState::Invalid,
            error: Some("no module.toml".into()),
        },
        ModuleStatus {
            folder: "spotify".into(),
            module: Some(ModuleInfo {
                id: ModuleId::try_new("com.example.spotify".to_owned()).unwrap(),
                name: "Spotify Now Playing".into(),
                description: Some("Album art.".into()),
                version: Some("1.0.0".into()),
                platforms: vec!["macos".into(), "linux".into()],
            }),
            enabled: true,
            state: ModuleState::Restarting,
            error: Some("exited with status 1".into()),
        },
    ])
    .await;
    assert_eq!(
        out,
        "broken (no valid module)\n  state:     invalid\n  error:     no module.toml\n\n\
         Spotify Now Playing 1.0.0 (com.example.spotify)\n  folder:    spotify\n  about:     Album art.\n  platforms: macos, linux\n  state:     restarting\n  error:     exited with status 1\n"
    );
}

#[tokio::test]
async fn no_agent_is_an_error() {
    let mut out = Vec::new();
    let failed = execute(
        list(),
        Err(ConnectError::Endpoint(std::io::ErrorKind::NotFound.into())),
        &mut out,
    );
    let error = tokio::time::timeout(WAIT, failed)
        .await
        .expect("the command fails in time")
        .unwrap_err();
    assert!(format!("{error:#}").contains("running, compatible OpenLogi agent"));
}
