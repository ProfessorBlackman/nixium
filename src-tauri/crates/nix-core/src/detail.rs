// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Per-process detail and the process tree. `PRC-3`, `PRC-4`.
//!
//! # Most of this is unreadable for other users' processes
//!
//! Measured rather than assumed. For a process belonging to somebody else:
//!
//! | | Own process | Another user's |
//! | --- | --- | --- |
//! | `io` — bytes read and written | readable | **no** |
//! | `environ` — the environment | readable | **no** |
//! | `fd/` — open files | readable | **no** |
//! | `cgroup` | readable | readable |
//! | `task/` — threads | readable | readable |
//!
//! So a detail panel for `systemd` can show its cgroup and its threads and nothing else. The important
//! thing is that it **says so**, rather than rendering empty sections that look like a process with no
//! open files and no environment. [`Detail::restricted`] carries that fact, and the reason.
//!
//! Reading another user's environment is genuinely sensitive — it routinely holds credentials — so
//! this is one place where nix does *not* offer to escalate. A task manager that will show you any
//! process's secrets for a password is a credential-harvesting tool with a nice icon.
//!
//! # Disk footprint is the link back to the storage half
//!
//! The specification asks for it, and the honest version is the size of what the process actually has
//! open plus its executable — not a guess at "how much disk this program uses", which is a question
//! about packages rather than processes and belongs to `PKG-1`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::process::Process;

/// Bytes a process has moved, from `/proc/<pid>/io`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProcessIo {
    /// Bytes read through syscalls, whether or not they touched a disk.
    #[ts(type = "number")]
    pub read_chars: u64,
    #[ts(type = "number")]
    pub written_chars: u64,
    /// Bytes that actually came from a block device. Zero for a process working entirely from cache,
    /// which is why both figures are shown: they answer different questions.
    #[ts(type = "number")]
    pub read_bytes: u64,
    #[ts(type = "number")]
    pub written_bytes: u64,
}

/// One file a process has open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OpenFile {
    /// The file descriptor number.
    #[ts(type = "number")]
    pub fd: u32,
    /// Where it points. A socket or pipe appears as `socket:[…]`, which is the kernel's own notation.
    pub target: String,
    /// Size in bytes, for the ones that are real files.
    #[ts(type = "number | null")]
    pub bytes: Option<u64>,
}

impl OpenFile {
    /// Whether this is a real file rather than a socket, pipe or device.
    #[must_use]
    pub fn is_regular(&self) -> bool {
        self.target.starts_with('/') && self.bytes.is_some()
    }
}

/// Everything the detail panel can show about one process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Detail {
    #[ts(type = "number")]
    pub pid: u32,
    pub executable: Option<PathBuf>,
    pub working_directory: Option<PathBuf>,
    /// The systemd slice or container this belongs to. Readable for any process.
    pub cgroup: Option<String>,
    #[ts(type = "number")]
    pub thread_count: u32,
    pub io: Option<ProcessIo>,
    pub open_files: Option<Vec<OpenFile>>,
    /// Environment variables. **Never escalated to read**, because another user's environment
    /// routinely contains credentials.
    pub environment: Option<Vec<(String, String)>>,
    /// Sum of the sizes of the regular files it has open, plus its executable.
    #[ts(type = "number | null")]
    pub disk_footprint: Option<u64>,
    /// Why parts of this are missing, when they are. Empty when everything was readable.
    pub restricted: Vec<String>,
}

/// Parse `/proc/<pid>/io`.
#[must_use]
pub fn parse_io(text: &str) -> ProcessIo {
    let field = |name: &str| -> u64 {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.trim_start_matches(':').trim().parse().ok())
            .unwrap_or(0)
    };
    ProcessIo {
        read_chars: field("rchar"),
        written_chars: field("wchar"),
        read_bytes: field("read_bytes"),
        written_bytes: field("written_bytes"),
    }
}

/// Parse `/proc/<pid>/cgroup`, taking the unified hierarchy's path.
///
/// Lines are `hierarchy:controllers:path`. On a cgroup-v2 system there is one line whose hierarchy is
/// `0` and whose controller list is empty, which is the one that means anything.
#[must_use]
pub fn parse_cgroup(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| {
            let mut parts = line.splitn(3, ':');
            let hierarchy = parts.next()?;
            let _controllers = parts.next()?;
            let path = parts.next()?;
            (hierarchy == "0").then(|| path.to_string())
        })
        .or_else(|| {
            // A cgroup-v1 system has no `0:` line; the first entry's path is still informative.
            text.lines()
                .next()
                .and_then(|line| line.splitn(3, ':').nth(2))
                .map(str::to_string)
        })
        .filter(|path| !path.is_empty())
}

/// Parse `/proc/<pid>/environ`, which is NUL-separated `KEY=VALUE`.
#[must_use]
pub fn parse_environ(raw: &[u8]) -> Vec<(String, String)> {
    String::from_utf8_lossy(raw)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Read everything readable about one process.
#[must_use]
pub fn read(root: &Path, pid: u32) -> Detail {
    let dir = root.join(pid.to_string());
    let mut restricted = Vec::new();

    let io = std::fs::read_to_string(dir.join("io"))
        .ok()
        .map(|text| parse_io(&text));
    if io.is_none() {
        restricted
            .push("Bytes read and written are only visible for your own processes.".to_string());
    }

    let open_files = read_open_files(&dir);
    if open_files.is_none() {
        restricted.push("Open files are only visible for your own processes.".to_string());
    }

    let environment = std::fs::read(dir.join("environ"))
        .ok()
        .map(|raw| parse_environ(&raw));
    if environment.is_none() {
        restricted.push(
            "The environment is only visible for your own processes, and nix will not ask for \
             administrator rights to read it — another user's environment routinely holds passwords."
                .to_string(),
        );
    }

    let executable = std::fs::read_link(dir.join("exe")).ok();
    let executable_bytes = executable
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len());

    let disk_footprint = open_files.as_ref().map(|files| {
        let open: u64 = files.iter().filter_map(|f| f.bytes).sum();
        open + executable_bytes.unwrap_or(0)
    });

    Detail {
        pid,
        executable,
        working_directory: std::fs::read_link(dir.join("cwd")).ok(),
        cgroup: std::fs::read_to_string(dir.join("cgroup"))
            .ok()
            .and_then(|text| parse_cgroup(&text)),
        thread_count: u32::try_from(
            std::fs::read_dir(dir.join("task"))
                .map(|entries| entries.flatten().count())
                .unwrap_or(0),
        )
        .unwrap_or(0),
        io,
        open_files,
        environment,
        disk_footprint,
        restricted,
    }
}

/// The files a process has open, or `None` when the directory cannot be listed.
fn read_open_files(dir: &Path) -> Option<Vec<OpenFile>> {
    let entries = std::fs::read_dir(dir.join("fd")).ok()?;
    let mut files: Vec<OpenFile> = entries
        .flatten()
        .filter_map(|entry| {
            let fd: u32 = entry.file_name().to_str()?.parse().ok()?;
            let target = std::fs::read_link(entry.path()).ok()?;
            let target = target.to_string_lossy().into_owned();
            // Following the link to size it: a socket or a pipe has no size, and `metadata` on the
            // descriptor itself would report the pipe rather than a file.
            let bytes = if target.starts_with('/') {
                std::fs::metadata(&target).ok().map(|meta| meta.len())
            } else {
                None
            };
            Some(OpenFile { fd, target, bytes })
        })
        .collect();
    files.sort_by_key(|f| f.fd);
    Some(files)
}

/// One node of the process tree. `PRC-4`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TreeNode {
    #[ts(type = "number")]
    pub pid: u32,
    pub name: String,
    /// This process alone.
    pub cpu_percent: f32,
    #[ts(type = "number")]
    pub memory_bytes: u64,
    /// This process **and everything below it**, which is the figure that explains a slow machine: a
    /// build system's cost is spread across dozens of short-lived children, and each one alone looks
    /// like nothing.
    pub subtree_cpu_percent: f32,
    #[ts(type = "number")]
    pub subtree_memory_bytes: u64,
    #[ts(type = "number")]
    pub descendants: u32,
    pub children: Vec<TreeNode>,
}

/// Build the process tree, with subtree totals. `PRC-4`.
///
/// # Cycles, and what they actually do
///
/// A list built from `ppid` is a snapshot taken over several milliseconds, so it can contain a child
/// whose recorded parent has already exited, or in principle a pair that each claim the other.
///
/// The obvious worry is infinite recursion, and it turns out **not** to be the problem: every process
/// has exactly one parent, so a cycle cannot be reached from a root — no member of a cycle has a
/// parent outside it. The real failure is quieter. A cyclic pair is reachable from *nothing*, so
/// walking down from the roots simply **never visits it**, and those processes vanish from the tree
/// without a trace.
///
/// That was a live defect here, found because the test written to check the recursion passed with the
/// guard deliberately removed — it had built a cycle that was unreachable, so the code under test
/// never ran.
///
/// So the walk does two things: it refuses to visit a pid twice, which is what makes the *second*
/// pass safe, and then it takes anything still unvisited as a root of its own. Every process appears
/// exactly once, whatever `ppid` claims.
#[must_use]
pub fn tree(processes: &[Process]) -> Vec<TreeNode> {
    let mut children: HashMap<u32, Vec<&Process>> = HashMap::new();
    let known: std::collections::HashSet<u32> = processes.iter().map(|p| p.pid).collect();
    for process in processes {
        children.entry(process.ppid).or_default().push(process);
    }

    // A root is a process whose parent is not in the list — pid 1, kernel threads under pid 2, and
    // anything orphaned between the listing and now.
    let mut roots: Vec<&Process> = processes
        .iter()
        .filter(|p| !known.contains(&p.ppid) || p.ppid == p.pid)
        .collect();
    roots.sort_by_key(|a| a.pid);

    let mut seen = std::collections::HashSet::new();
    let mut nodes: Vec<TreeNode> = roots
        .into_iter()
        .filter_map(|root| build(root, &children, &mut seen))
        .collect();

    // Anything the walk could not reach: a cyclic pair, or a group orphaned in a way that leaves no
    // entry point. Adopted as roots in pid order so the result is stable, and the `seen` guard inside
    // `build` is what stops the cycle unrolling forever now that it *is* entered.
    let mut stranded: Vec<&Process> = processes
        .iter()
        .filter(|p| !seen.contains(&p.pid))
        .collect();
    stranded.sort_by_key(|p| p.pid);
    for process in stranded {
        if let Some(node) = build(process, &children, &mut seen) {
            nodes.push(node);
        }
    }
    nodes
}

fn build(
    process: &Process,
    children: &HashMap<u32, Vec<&Process>>,
    seen: &mut std::collections::HashSet<u32>,
) -> Option<TreeNode> {
    // The cycle guard. Also stops a process listed twice from appearing twice.
    if !seen.insert(process.pid) {
        return None;
    }

    let mut built: Vec<TreeNode> = children
        .get(&process.pid)
        .into_iter()
        .flatten()
        // A process cannot be its own child, however `ppid` reads.
        .filter(|child| child.pid != process.pid)
        .filter_map(|child| build(child, children, seen))
        .collect();
    built.sort_by(|a, b| b.subtree_cpu_percent.total_cmp(&a.subtree_cpu_percent));

    let descendants = built.iter().map(|c| c.descendants + 1).sum();
    let subtree_cpu_percent =
        process.cpu_percent + built.iter().map(|c| c.subtree_cpu_percent).sum::<f32>();
    let subtree_memory_bytes =
        process.memory_bytes + built.iter().map(|c| c.subtree_memory_bytes).sum::<u64>();

    Some(TreeNode {
        pid: process.pid,
        name: process.name.clone(),
        cpu_percent: process.cpu_percent,
        memory_bytes: process.memory_bytes,
        subtree_cpu_percent,
        subtree_memory_bytes,
        descendants,
        children: built,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::process::ProcessState;

    fn process(pid: u32, ppid: u32, name: &str, cpu: f32, memory: u64) -> Process {
        Process {
            pid,
            ppid,
            name: name.to_string(),
            command: format!("/usr/bin/{name}"),
            state: ProcessState::Sleeping,
            uid: 1000,
            user: "alice".to_string(),
            cpu_percent: cpu,
            memory_bytes: memory,
            threads: 1,
            started_ticks: 100,
        }
    }

    /// Golden output, §P8. Captured from this machine.
    #[test]
    fn io_is_parsed_from_real_output() {
        let io = parse_io(
            "rchar: 402304\nwchar: 511\nsyscr: 75\nsyscw: 6\nread_bytes: 0\nwrite_bytes: 0\ncancelled_write_bytes: 0\n",
        );
        assert_eq!(io.read_chars, 402_304);
        assert_eq!(io.written_chars, 511);
        assert_eq!(
            io.read_bytes, 0,
            "a process working from cache moved no blocks, which is why both figures are kept"
        );
    }

    #[test]
    fn a_cgroup_v2_path_is_taken_from_the_unified_hierarchy() {
        let cgroup = parse_cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.gnome.Terminal.slice/vte-spawn-3e7f40b4.scope\n",
        )
        .unwrap();
        assert!(cgroup.starts_with("/user.slice/"));
        assert!(!cgroup.contains(':'), "the path only, not the whole line");
    }

    #[test]
    fn a_cgroup_v1_system_still_yields_something() {
        let cgroup = parse_cgroup("11:memory:/system.slice/sshd.service\n5:cpu:/system.slice\n");
        assert_eq!(
            cgroup.as_deref(),
            Some("/system.slice/sshd.service"),
            "no 0: line, so the first entry's path is the informative one"
        );
    }

    #[test]
    fn an_empty_or_malformed_cgroup_is_none() {
        assert_eq!(parse_cgroup(""), None);
        assert_eq!(parse_cgroup("0::\n"), None, "an empty path is not a cgroup");
        assert_eq!(parse_cgroup("nonsense\n"), None);
    }

    #[test]
    fn the_environment_is_parsed_from_nul_separated_pairs() {
        let env = parse_environ(b"PATH=/usr/bin\0HOME=/home/alice\0MALFORMED\0EMPTY=\0");
        assert_eq!(env.len(), 3, "an entry with no `=` is not a variable");
        assert_eq!(env[0], ("PATH".to_string(), "/usr/bin".to_string()));
        assert_eq!(
            env[2],
            ("EMPTY".to_string(), String::new()),
            "a variable set to nothing is still set"
        );
    }

    // ---- the tree, `PRC-4` ----

    /// The figure that explains a slow machine: a build's cost is spread over its children.
    #[test]
    fn subtree_totals_aggregate_the_whole_branch() {
        let processes = vec![
            process(1, 0, "systemd", 0.5, 10),
            process(100, 1, "make", 1.0, 100),
            process(101, 100, "cc", 90.0, 500),
            process(102, 100, "cc", 80.0, 400),
        ];

        let tree = tree(&processes);
        assert_eq!(
            tree.len(),
            1,
            "one root: pid 1, whose parent 0 is not listed"
        );
        let root = &tree[0];
        assert_eq!(root.pid, 1);
        assert!(
            (root.subtree_cpu_percent - 171.5).abs() < 0.01,
            "{}",
            root.subtree_cpu_percent
        );
        assert_eq!(root.subtree_memory_bytes, 1010);
        assert_eq!(root.descendants, 3);

        let make = &root.children[0];
        assert_eq!(make.pid, 100);
        assert!((make.cpu_percent - 1.0).abs() < 0.01, "make alone is idle");
        assert!(
            (make.subtree_cpu_percent - 171.0).abs() < 0.01,
            "but its branch is doing the work: {}",
            make.subtree_cpu_percent
        );
        assert_eq!(make.descendants, 2);
    }

    #[test]
    fn branches_are_ordered_by_subtree_cost() {
        let processes = vec![
            process(1, 0, "systemd", 0.0, 0),
            process(10, 1, "quiet", 0.0, 0),
            process(20, 1, "busy-parent", 0.0, 0),
            process(21, 20, "busy-child", 99.0, 0),
        ];
        let tree = tree(&processes);
        assert_eq!(
            tree[0].children[0].pid, 20,
            "an idle parent with a busy child sorts above an idle one with none"
        );
    }

    /// # Regression
    ///
    /// The first version of this test built a cycle and asserted the walk did not recurse forever. It
    /// passed with the cycle guard **deliberately removed**, which meant it was testing nothing: in a
    /// single-parent graph no member of a cycle has a parent outside it, so a cycle is reachable from
    /// no root and the recursion never started.
    ///
    /// Working out why exposed the actual defect. Those processes were reachable from nothing, so they
    /// were dropped from the tree entirely — silently, and in violation of the invariant that every
    /// process appears exactly once.
    #[test]
    fn a_cycle_is_still_reported_and_still_terminates() {
        let processes = vec![
            process(100, 101, "a", 1.0, 10),
            process(101, 100, "b", 2.0, 20),
        ];

        let tree = tree(&processes);

        // Both appear, which is the property that was broken.
        let mut found = std::collections::HashSet::new();
        fn walk(node: &TreeNode, found: &mut std::collections::HashSet<u32>) {
            found.insert(node.pid);
            for child in &node.children {
                walk(child, found);
            }
        }
        for root in &tree {
            walk(root, &mut found);
        }
        assert_eq!(
            found.len(),
            2,
            "a cyclic pair must not vanish from the tree: {tree:?}"
        );

        // And each exactly once, which is what the guard is for now that the cycle is entered.
        let total: u32 = tree.iter().map(|n| n.descendants + 1).sum();
        assert_eq!(total, 2, "expanded more than once: {tree:?}");
    }

    /// A longer cycle, in case two happened to be a special case.
    #[test]
    fn a_three_process_cycle_terminates_and_is_reported() {
        let processes = vec![
            process(100, 102, "a", 1.0, 10),
            process(101, 100, "b", 1.0, 10),
            process(102, 101, "c", 1.0, 10),
        ];
        let tree = tree(&processes);
        let total: u32 = tree.iter().map(|n| n.descendants + 1).sum();
        assert_eq!(total, 3, "{tree:?}");
    }

    #[test]
    fn a_process_that_is_its_own_parent_is_a_root_and_not_its_own_child() {
        let processes = vec![
            process(1, 1, "init", 1.0, 10),
            process(2, 1, "child", 2.0, 20),
        ];
        let tree = tree(&processes);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].pid, 1);
        assert_eq!(tree[0].children.len(), 1, "only the real child");
        assert_eq!(tree[0].descendants, 1);
    }

    #[test]
    fn an_orphan_whose_parent_has_exited_is_a_root() {
        let processes = vec![
            process(1, 0, "systemd", 0.0, 0),
            // Its parent 999 exited between the listing and now.
            process(500, 999, "orphan", 5.0, 50),
        ];
        let tree = tree(&processes);
        assert_eq!(
            tree.len(),
            2,
            "it must appear somewhere rather than vanishing"
        );
        assert!(tree.iter().any(|n| n.pid == 500));
    }

    #[test]
    fn an_empty_list_makes_an_empty_tree() {
        assert!(tree(&[]).is_empty());
    }

    #[test]
    fn every_process_appears_exactly_once() {
        let processes: Vec<Process> = (1..=50)
            .map(|pid| process(pid, if pid == 1 { 0 } else { pid / 2 }, "p", 1.0, 10))
            .collect();

        let tree = tree(&processes);
        let mut counted = std::collections::HashMap::new();
        fn walk(node: &TreeNode, counted: &mut std::collections::HashMap<u32, u32>) {
            *counted.entry(node.pid).or_default() += 1;
            for child in &node.children {
                walk(child, counted);
            }
        }
        for root in &tree {
            walk(root, &mut counted);
        }
        assert_eq!(counted.len(), 50);
        assert!(
            counted.values().all(|n| *n == 1),
            "a process listed twice would be counted twice in every subtree above it"
        );
    }

    #[test]
    fn a_regular_file_is_distinguished_from_a_socket() {
        let file = OpenFile {
            fd: 3,
            target: "/home/alice/notes.txt".into(),
            bytes: Some(1024),
        };
        let socket = OpenFile {
            fd: 4,
            target: "socket:[12345]".into(),
            bytes: None,
        };
        assert!(file.is_regular());
        assert!(
            !socket.is_regular(),
            "a socket has no size and is not a file"
        );
    }

    // ---- against this machine ----

    /// Own process: everything readable.
    #[test]
    fn our_own_detail_is_fully_readable() {
        let detail = read(Path::new("/proc"), std::process::id());
        if detail.executable.is_none() && detail.cgroup.is_none() {
            return; // no /proc here
        }
        assert!(detail.io.is_some(), "our own io must be readable");
        assert!(detail.open_files.is_some());
        assert!(detail.environment.is_some());
        assert!(
            detail.restricted.is_empty(),
            "nothing should be restricted for our own process: {:?}",
            detail.restricted
        );
        assert!(detail.thread_count >= 1);
        assert!(detail.disk_footprint.is_some());
    }

    /// Another user's process: partial, and it says which parts and why.
    #[test]
    fn another_users_detail_explains_what_it_cannot_show() {
        if !Path::new("/proc/1/cgroup").exists() {
            return;
        }
        // Running as root would make everything readable, which is a different (valid) case.
        if std::fs::read_to_string("/proc/1/io").is_ok() {
            return;
        }

        let detail = read(Path::new("/proc"), 1);
        assert!(detail.io.is_none());
        assert!(detail.environment.is_none());
        assert!(
            detail.restricted.len() >= 2,
            "each missing section must say why: {:?}",
            detail.restricted
        );
        assert!(
            detail.restricted.iter().any(|r| r.contains("passwords")),
            "the environment's refusal is deliberate and should say so: {:?}",
            detail.restricted
        );
        // What *is* readable for any process still comes through.
        assert!(detail.cgroup.is_some(), "cgroup is readable for anything");
        assert!(detail.thread_count >= 1);
    }

    #[test]
    fn this_machines_tree_covers_every_process_once() {
        let mut sampler = crate::process::ProcessSampler::new();
        let all = sampler.sample(std::time::Duration::from_secs(1));
        if all.is_empty() {
            return;
        }
        let tree = tree(&all);
        let total: u32 = tree.iter().map(|n| n.descendants + 1).sum();
        assert_eq!(
            total as usize,
            all.len(),
            "every process must appear in the tree exactly once"
        );
    }
}
