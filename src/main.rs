use log::{debug, error, info};
use tentacle::multiaddr::Multiaddr;
use tokio::signal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::task_tracker::TaskTracker;

use std::str::FromStr;

use ckb_pcn_node::ckb::Command;
use ckb_pcn_node::{start_ckb, start_ldk, start_rpc, Config};

#[derive(Debug, Clone)]
struct TaskTrackerWithCancellation {
    tracker: TaskTracker,
    token: CancellationToken,
}

impl TaskTrackerWithCancellation {
    fn new() -> Self {
        Self {
            tracker: TaskTracker::new(),
            token: CancellationToken::new(),
        }
    }

    async fn close(&self) {
        self.token.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }
}

static TOKIO_TASK_TRACKER_WITH_CANCELLATION: once_cell::sync::Lazy<TaskTrackerWithCancellation> =
    once_cell::sync::Lazy::new(TaskTrackerWithCancellation::new);

/// Create a new CancellationToken for exit signal
fn new_tokio_cancellation_token() -> CancellationToken {
    TOKIO_TASK_TRACKER_WITH_CANCELLATION.token.clone()
}

/// Create a new TaskTracker to track task progress
fn new_tokio_task_tracker() -> TaskTracker {
    TOKIO_TASK_TRACKER_WITH_CANCELLATION.tracker.clone()
}

#[tokio::main]
pub async fn main() {
    env_logger::init();

    let config = Config::parse();
    debug!("Parsed config: {:?}", &config);

    match config {
        Config { ckb, ldk, rpc } => {
            if let Some(ldk_config) = ldk {
                info!("Starting ldk");
                start_ldk(ldk_config).await;
            }
            match (ckb, rpc) {
                (None, Some(_)) => {
                    error!("Rpc service requires ckb service to be started. Exiting.");
                    return;
                }
                (None, None) => {
                    // No service to run, skipping.
                }
                (Some(ckb_config), rpc) => {
                    const CHANNEL_SIZE: usize = 4000;
                    let (command_sender, command_receiver) = mpsc::channel(CHANNEL_SIZE);
                    assert!(
                        ckb_config.bootnode_addrs.len() < CHANNEL_SIZE,
                        "Too many bootnodes ({} allowed, having {})",
                        CHANNEL_SIZE,
                        ckb_config.bootnode_addrs.len()
                    );
                    for bootnode in &ckb_config.bootnode_addrs {
                        let addr = Multiaddr::from_str(bootnode).expect("valid bootnode");
                        let command = Command::Connect(addr);
                        command_sender
                            .send(command)
                            .await
                            .expect("receiver not closed")
                    }

                    info!("Starting ckb");
                    start_ckb(
                        ckb_config,
                        command_receiver,
                        new_tokio_cancellation_token(),
                        new_tokio_task_tracker(),
                    )
                    .await;

                    // Start rpc service
                    if let Some(rpc_config) = rpc {
                        let shutdown_signal = async {
                            let token = new_tokio_cancellation_token();
                            token.cancelled().await;
                        };
                        new_tokio_task_tracker().spawn(async move {
                            start_rpc(rpc_config, command_sender, shutdown_signal).await;
                        });
                    };
                }
            }
        }
    }

    signal::ctrl_c().await.expect("Failed to listen for event");
    TOKIO_TASK_TRACKER_WITH_CANCELLATION.close().await;
}
