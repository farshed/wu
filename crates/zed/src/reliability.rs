use gpui::{App, AppContext, Entity};
use project::worktree_store::WorktreeStoreDiagnostics;
use smol::stream::StreamExt;
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use util::ResultExt;
use workspace::WorkspaceStore;


pub fn init(workspace_store: Entity<WorkspaceStore>, cx: &mut App) {
    start_memory_usage_logging(workspace_store, cx);
}

const MEMORY_USAGE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const MEMORY_USAGE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10 * 60);
const MEMORY_USAGE_MINIMUM_LOGGED_DELTA: u64 = 64 * 1024 * 1024;

/// Periodically logs this process' memory usage, so that gradual memory growth can be
///
/// Logs on a fixed heartbeat, and additionally whenever resident memory changed
/// significantly since the last logged value, so that bursts of growth are timestamped
/// against the surrounding log entries.
fn start_memory_usage_logging(workspace_store: Entity<WorkspaceStore>, cx: &App) {
    let (diagnostics_sender, mut diagnostics_receiver) = futures::channel::mpsc::unbounded();
    cx.spawn(async move |cx| {
        while diagnostics_receiver.next().await.is_some() {
            cx.update(|cx| log_worktree_diagnostics(&workspace_store, cx));
        }
    })
    .detach();

    let executor = cx.background_executor().clone();
    cx.background_spawn(async move {
        let Some(pid) = sysinfo::get_current_pid().log_err() else {
            return;
        };
        let refresh_kind = ProcessRefreshKind::nothing().with_memory();
        let mut system = System::new();
        let mut last_logged_resident: Option<u64> = None;
        let mut last_logged_at = Instant::now();
        loop {
            let refreshed = system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                false,
                refresh_kind,
            );
            if refreshed == 1
                && let Some(process) = system.process(pid)
            {
                let resident = process.memory();
                let significant_change = last_logged_resident.is_none_or(|last| {
                    resident.abs_diff(last) >= (last / 10).max(MEMORY_USAGE_MINIMUM_LOGGED_DELTA)
                });
                if significant_change || last_logged_at.elapsed() >= MEMORY_USAGE_HEARTBEAT_INTERVAL
                {
                    const MIB: u64 = 1024 * 1024;
                    let delta = match last_logged_resident {
                        Some(last) => {
                            format!(" ({:+} MiB)", (resident as i64 - last as i64) / MIB as i64)
                        }
                        None => String::new(),
                    };
                    log::info!(
                        "memory usage: resident {} MiB{delta}, virtual {} MiB",
                        resident / MIB,
                        process.virtual_memory() / MIB,
                    );
                    if diagnostics_sender.unbounded_send(()).is_err() {
                        return;
                    }
                    last_logged_resident = Some(resident);
                    last_logged_at = Instant::now();
                }
            }
            executor.timer(MEMORY_USAGE_POLL_INTERVAL).await;
        }
    })
    .detach();
}

fn log_worktree_diagnostics(workspace_store: &Entity<WorkspaceStore>, cx: &App) {
    let workspaces = workspace_store
        .read(cx)
        .workspaces()
        .filter_map(|workspace| workspace.upgrade())
        .collect::<Vec<_>>();
    let mut worktree_store_ids = HashSet::new();
    let mut store_count = 0;
    let mut aggregate = WorktreeStoreDiagnostics::default();

    for workspace in workspaces {
        let project = workspace.read(cx).project().clone();
        let worktree_store = project.read(cx).worktree_store();
        if !worktree_store_ids.insert(worktree_store.entity_id()) {
            continue;
        }
        store_count += 1;

        let WorktreeStoreDiagnostics {
            worktree_slots,
            live_worktrees,
            visible_worktrees,
            strong_handles,
            dead_weak_handles,
            loading_worktrees,
            total_entries,
            visible_entries,
            largest_worktree,
        } = worktree_store.read(cx).diagnostics(cx);
        aggregate.worktree_slots += worktree_slots;
        aggregate.live_worktrees += live_worktrees;
        aggregate.visible_worktrees += visible_worktrees;
        aggregate.strong_handles += strong_handles;
        aggregate.dead_weak_handles += dead_weak_handles;
        aggregate.loading_worktrees += loading_worktrees;
        aggregate.total_entries += total_entries;
        aggregate.visible_entries += visible_entries;

        if let Some(largest_worktree) = largest_worktree
            && aggregate
                .largest_worktree
                .as_ref()
                .is_none_or(|largest| largest_worktree.entries > largest.entries)
        {
            aggregate.largest_worktree = Some(largest_worktree);
        }
    }

    let WorktreeStoreDiagnostics {
        worktree_slots,
        live_worktrees,
        visible_worktrees,
        strong_handles,
        dead_weak_handles,
        loading_worktrees,
        total_entries,
        visible_entries,
        largest_worktree,
    } = aggregate;
    match largest_worktree {
        Some(largest_worktree) => log::info!(
            "worktree diagnostics: stores {store_count}, slots {worktree_slots}, live {live_worktrees}, visible {visible_worktrees}, strong {strong_handles}, dead weak {dead_weak_handles}, loading {loading_worktrees}, entries {total_entries}, visible entries {visible_entries}, largest {} ({} entries, {} visible)",
            largest_worktree.path.display(),
            largest_worktree.entries,
            largest_worktree.visible_entries,
        ),
        None => log::info!(
            "worktree diagnostics: stores {store_count}, slots {worktree_slots}, live {live_worktrees}, visible {visible_worktrees}, strong {strong_handles}, dead weak {dead_weak_handles}, loading {loading_worktrees}, entries {total_entries}, visible entries {visible_entries}, largest none",
        ),
    }
}
