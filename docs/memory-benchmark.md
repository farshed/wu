# Linux memory benchmark — 2026-09-19

## Releases and installation

These were the latest stable GitHub releases when checked on 2026-09-19:

| Editor | Release | Release commit | Installation |
| --- | --- | --- | --- |
| Wu | [1.0.8](https://github.com/farshed/wu/releases/tag/v1.0.8) | `e456dff81e3efbf5cda01ddb29d052f6bc74e67c` | `/root/.local/wu.app` |
| Zed | [1.20.2](https://github.com/zed-industries/zed/releases/tag/v1.20.2) | `7c451e694f3c52ee0aeb01d7e28b5fa18cd0ad2f` | `/root/.local/zed.app` |

The official Linux ARM64 archives were extracted into `/root/.local`, with launcher symlinks in `/root/.local/bin`. Archive SHA-256 values matched GitHub's published asset digests:

```text
wu-linux-aarch64.tar.gz
f2761a0107366935874ffe046673194ad7f8e47ba63df27c62e6b82e6143119a

zed-linux-aarch64.tar.gz
715a5252234522bc9e8e4a8c1f9b462cf7bb2881eed23b7c8ae650b41c24aa6f
```

## Environment and method

Ubuntu 26.04 ARM64; kernel `7.0.0-29-generic`; six Neoverse-N1 virtual CPUs; 7.7 GiB RAM. Xvfb provides a 1280×800, 24-bit X11 display without a window manager or compositor. Both editors use Vulkan through Mesa 26.0.8 llvmpipe (LLVM 21.1.8), a CPU renderer. The benchmark ran as root with the editors' explicit root opt-in and isolated profiles.

The runner launches the actual `libexec/*-editor` binaries, follows descendants and session members, and retains observed descendant identities when they are reparented. It reads every surviving process's `/proc/PID/smaps_rollup`. **PSS** is the primary comparison: it apportions shared resident pages among processes. **RSS** is also reported, but summing it can count shared pages more than once. See the [Linux kernel documentation](https://docs.kernel.org/filesystems/proc.html) for the counters' definitions. All table values are MiB (1,048,576 bytes).

Each idle scenario uses three independent launches of each editor, alternating which editor runs first in each pair. Only one editor runs at a time. Every launch gets fresh user data and XDG config/data/cache/state directories, including fresh shader caches. After a visible window opens and is resized to 1280×800, the runner waits 30 seconds, samples ten times at one-second intervals, and reports the median. The scenario result is the median of the three run medians. These are settled-memory measurements, not startup peaks; the machine's filesystem cache is not flushed.

Automatic editor updates and telemetry are disabled identically. Other editor defaults remain enabled, including automatic installation of the HTML extension (0.3.2 in these runs). No accounts are signed in. The software-GPU warning is suppressed for both editors. Screenshots and window metadata verify that each editor rendered the intended workspace.

- **Welcome:** finish onboarding with Ctrl+Enter and leave the welcome tab open.
- **Project:** open `/root/wu` (checkout `82c0588`, 3,517 tracked files) with its project tree visible, project trusted in the disposable profile, and no source file open. Wu reads its existing `.wu/settings.json`; Zed has no `.zed/settings.json` in this checkout. No build or language-server workload is deliberately started.

The totals include the editors and any running descendants, including in-process extension/runtime/rendering allocations. They exclude Xvfb, the benchmark harness, the shared D-Bus/desktop services, unmapped filesystem cache, and kernel allocations. This is an editor process-memory comparison, not the entire desktop session's RAM use.

## Idle results

Wu used about **39–42 MiB less PSS (9.4–9.9%)** in these two idle scenarios.

| Scenario | Wu PSS | Zed PSS | Wu PSS reduction | Wu summed RSS | Zed summed RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Welcome window | 380.2 | 419.6 | 9.4% | 415.9 | 484.5 |
| Wu repository open | 380.8 | 422.4 | 9.9% | 416.4 | 487.2 |

PSS medians for each independent launch:

| Scenario | Editor | Run 1 | Run 2 | Run 3 |
| --- | --- | ---: | ---: | ---: |
| Welcome | Wu | 378.5 | 380.2 | 383.9 |
| Welcome | Zed | 419.4 | 419.6 | 420.0 |
| Project | Wu | 381.2 | 380.8 | 379.8 |
| Project | Zed | 421.9 | 423.4 | 422.4 |

All 12 launches completed, yielding 120 samples. Swap PSS was zero throughout. Every sample included one Wu process or two Zed processes: the Zed editor and its crash handler. For example, the first Zed welcome sample comprised 398.0 MiB editor PSS plus 21.5 MiB crash-handler PSS. The child is included in all Zed totals.

Both editors finished scanning the same 4,069 project entries in all project runs. No language-server process was present during sampling. Summed RSS was 14.2% lower for Wu at the welcome screen and 14.5% lower with the project open; PSS is preferred because RSS includes duplicate accounting for shared pages.

Validation included actual rendered-window screenshots, log/window checks, and a synthetic parent/child process test demonstrating that child allocations are counted, process totals are summed, exited editors are rejected, and descendants are stopped. All editor and Xvfb processes exited after the benchmark. Bash syntax and Git whitespace checks passed.

## Active workload protocol

The extended Linux runner adds `--scenario workspace` and `--scenario rust`. Each is a sequence in one editor session, so later stages include memory retained from earlier actions. Every editor/run gets a new disposable project and profile. Source-file hashes are recorded in `workload.json`; the user's checkout is never edited by the UI workload.

The workspace snapshot retains the repository's `.wu/settings.json`, which Zed does not read; no corresponding `.zed/settings.json` is added. Its extra excluded fixture directory is absent from this snapshot, and the selected searches return the same result counts. Other product/project defaults, including panel layouts, remain product-specific. The generated Rust project has no editor-specific project settings.

| Suite | Stage | Actions |
| --- | --- | --- |
| Workspace | Multiple files | Open 20 real Rust source files from a Git archive of this repository's HEAD, with preview tabs disabled and language servers disabled. |
| Workspace | Editing | Insert and save 20 distinct comments in a scratch Rust file, scrolling between edits; verify each comment reaches disk. |
| Workspace | Project search | Search for `pub struct Rope`, `fn offset_to_point`, and `fn focus_handle` across the source snapshot. |
| Workspace | Large file | Open a generated 200,000-line log, repeatedly navigate between its ends, and find request 123456. |
| Workspace | Terminal | Run a program that prints 20,000 lines in the integrated terminal, with 10,000 lines of scrollback. Verify its completion marker. |
| Workspace | Close tabs | Hide the terminal and close editor tabs, then measure retained memory. The project and terminal session remain open; Wu's search panel remains visible, while Zed's search tab closes with the other tabs. This is not an identical empty state. |
| Rust | Files and indexing | Open 11 files in a generated dependency-free Rust application with 30 modules and 3,000 functions. Allow 20 extra seconds for initial language-server indexing. |
| Rust | Editing | Perform 20 verified edit/save/scroll cycles with rust-analyzer and checking on save enabled. |
| Rust | Language features | Navigate from `main.rs` to `account.rs` using go-to-definition, request `String::` completions, introduce a type mismatch, and open diagnostics. |
| Rust | Tests in terminal | Remove the deliberate error and run `cargo test --offline` in the integrated terminal. Require a successful exit before measuring the settled state. |

Both editors use Rust toolchain `1.97.1-aarch64-unknown-linux-gnu` and `rust-analyzer 1.97.1 (8bab26f 2026-07-14)`. The rust-analyzer executable's SHA-256 is `50ceb44798c086191b09f0e72cbd963dfafcaacb5a8fc436c6204b28d51e5c33`. The process-tree totals include rust-analyzer, its proc-macro helper, the terminal shell, and any compiler/build processes alive at the time of a sample. Language servers are disabled only in the workspace suite to separate editor operations from the generated Rust application's language-server workload.

The terminal commands use a small Python driver to verify successful completion. Its memory is included while that child process is running. The log file is exactly 23,000,000 bytes (21.9 MiB).

The runner samples concurrently with the UI actions. The measured active runs use a 10-second initial wait, a nominal 0.5-second sampling interval, a five-second settling period after each action sequence, and six final samples per stage. The settled result is the median of the three per-run final-sample medians; the reported sampled peak is the median of the three per-run stage maxima. A sampled peak can miss allocations shorter than the sampling interval and excludes initial launch before the actions begin; it is not an exact high-water mark. Screenshots are retained during key actions and after each stage. File opening uses each editor's CLI connection; text insertion uses clipboard paste so software-rendering delays do not drop typed characters. Clipboard and CLI helper processes belong to the harness and are excluded from editor totals.

Run the two suites sequentially, with only one editor active during measurement:

```sh
ZED_ALLOW_ROOT=true ZED_ALLOW_EMULATED_GPU=1 \
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
xvfb-run -a -s '-screen 0 1280x800x24 -nolisten tcp' \
dbus-run-session -- script/memory-benchmark \
  --scenario workspace --runs 3 --settle 10 --phase-settle 5 \
  --samples 6 --interval 0.5
```

Repeat with `--scenario rust --rust-toolchain 1.97.1-aarch64-unknown-linux-gnu`. The active suites additionally require `xclip` and ImageMagick; the Rust suite requires `rustup`, an installed toolchain with `rust-src` and `rust-analyzer`, and Cargo. `--rust-toolchain` and `--rust-analyzer` can pin alternatives. No external Cargo dependencies are downloaded.

Initial setup runs are kept separately under `active-smoke-*` and excluded from the comparison. Very broad search queries (`pub fn`, `impl`, `cx.notify`) left Wu unable to service the next file-open request within the 30-second timeout in one setup run. The repeatable suite uses targeted symbol queries instead. This setup timeout is not a controlled cross-editor result or a diagnosis of its cause.

## Active results

All values below are total process-tree PSS in MiB. These are sequential workflow stages, not isolated feature costs. "Peak" means the median of each run's sampled stage maximum, as defined above.

### Workspace suite

| Stage | Wu settled | Zed settled | Wu peak | Zed peak | Wu settled reduction |
| --- | ---: | ---: | ---: | ---: | ---: |
| 20 source files open | 520.2 | 553.4 | 520.2 | 553.4 | 6.0% |
| Edit/save/scroll | 521.0 | 556.6 | 521.1 | 556.7 | 6.4% |
| Repository search | 1234.5 | 1380.3 | 1234.6 | 1473.3 | 10.6% |
| Large log navigation/search | 1466.0 | 1627.2 | 1550.0 | 1723.9 | 9.9% |
| Terminal output | 1496.7 | 1657.8 | 1504.1 | 1657.8 | 9.7% |
| Tabs closed, retained state | 1473.7 | 1632.3 | 1496.7 | 1657.9 | 9.7% |

Per-run settled PSS medians (runs 1, 2, 3):

| Stage | Wu | Zed |
| --- | --- | --- |
| Source files | 520.2, 520.2, 516.4 | 555.8, 553.4, 552.8 |
| Editing | 522.1, 521.0, 518.7 | 556.8, 556.4, 556.6 |
| Search | 1235.0, 1234.2, 1234.5 | 1425.0, 1375.5, 1380.3 |
| Large log | 1496.2, 1466.0, 1459.5 | 1669.2, 1595.8, 1627.2 |
| Terminal | 1526.3, 1496.7, 1491.3 | 1697.8, 1626.7, 1657.8 |
| Tabs closed | 1501.0, 1473.7, 1470.5 | 1664.2, 1593.3, 1632.3 |

All six sessions and 36 stages completed, producing 1,588 samples. Swap PSS was zero throughout. All 3,519 fixture-file hashes matched across the six fresh copies. Only the intended scratch-file edits remained in each project. The final search returned 157 matches across 122 files. Closing tabs did not return memory to the fresh-window baseline; retained search/terminal state and allocator caches mean this result alone does not establish a leak.

Raw results, including RSS, individual processes, samples, fixture hashes, and screenshots: `/root/.local/state/wu-memory-benchmark/20260919-active-workspace/`.

### Rust language-server suite

| Stage | Wu settled | Zed settled | Wu peak | Zed peak | Wu settled reduction |
| --- | ---: | ---: | ---: | ---: | ---: |
| Files and indexing | 1142.1 | 1174.1 | 1475.0 | 1526.0 | 2.7% |
| Edit/save/scroll with checking | 1146.8 | 1176.7 | 1371.6 | 1412.7 | 2.5% |
| Definition/completion/diagnostics | 1201.1 | 1223.4 | 1413.9 | 1416.3 | 1.8% |
| Fix error and run Cargo tests | 1205.5 | 1227.3 | 1429.9 | 1460.6 | 1.8% |

Per-run settled PSS medians (runs 1, 2, 3):

| Stage | Wu | Zed |
| --- | --- | --- |
| Files and indexing | 1152.8, 1120.3, 1142.1 | 1174.1, 1185.4, 1172.9 |
| Editing | 1156.3, 1122.9, 1146.8 | 1176.7, 1188.6, 1175.6 |
| Language features | 1201.1, 1177.9, 1202.8 | 1223.4, 1234.8, 1220.7 |
| Cargo tests | 1205.5, 1184.7, 1207.7 | 1227.3, 1238.3, 1224.9 |

All six sessions and 24 stages completed, producing 1,361 samples with zero swap PSS. All 34 initial source-file hashes matched across the six projects. Each session saved all 20 intended edits, removed the deliberately introduced type error, and successfully ran its unit test. Representative screenshots for both editors show the completion menu, E0308 diagnostics, and successful test output. Go-to-definition is checked against the destination file's window title. Sampled process trees include rust-analyzer, its proc-macro helper, Cargo, rustc, terminal shells, and, where caught by the sampling interval, linker processes. Every recorded total was checked against its constituent process counters, and phase medians/maxima were recomputed from the raw samples.

The language server dominates this fixture's footprint. For example, the last settled sample after loading files in run 1 was:

| Process | Wu session PSS | Zed session PSS |
| --- | ---: | ---: |
| Main editor | 405.0 | 426.6 |
| Crash handler | — | 21.5 |
| rust-analyzer | 731.3 | 709.5 |
| Proc-macro helper | 16.5 | 16.5 |

This is one illustrative sample, not another aggregate comparison. Despite using the same binary and source fixture, rust-analyzer's memory varies between launches. The smaller total percentage difference in this suite should not be interpreted as an isolated measure of editor-core savings. Three repetitions describe these runs but do not establish statistical significance or a universal percentage advantage; in particular, the language-feature sampled peaks are nearly equal.

Raw results: `/root/.local/state/wu-memory-benchmark/20260919-active-rust/`.

Together the active suites completed 12 editor sessions, 60 measured stages, and 2,949 samples. Wu's settled PSS was 6.0–10.6% lower in the workspace stages and 1.8–2.7% lower in the Rust stages. All benchmark editor, language-server, and Xvfb processes exited afterward. README has no changes.

## Reproduce the idle baseline

The Linux runner needs Python 3.11+, `xvfb-run`, `xauth`, `dbus-run-session`, `xdotool`, `xwininfo`, and a working Vulkan driver. ImageMagick's `import` is optional for screenshots. This VPS uses `/usr/share/vulkan/icd.d/lvp_icd.json` to select the software renderer.

From the repository root:

```sh
ZED_ALLOW_ROOT=true ZED_ALLOW_EMULATED_GPU=1 \
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
xvfb-run -a -s '-screen 0 1280x800x24 -nolisten tcp' \
dbus-run-session -- script/memory-benchmark \
  --runs 3 --settle 30 --samples 10
```

Append `--path /root/wu --trust-project` for the project scenario. `--trust-project` enables project trust only inside the newly created benchmark profiles. Omit `ZED_ALLOW_ROOT=true` when running as a normal user. `--wu` and `--zed` override editor binary paths; use `libexec/*-editor`, not the launcher. `--output` selects a new artifact directory, which must not already exist. Run `script/memory-benchmark --help` on Linux for all options; the existing macOS benchmark remains available on macOS.

Raw measurements, executable hashes, versions, logs, profiles, and screenshots are retained on this VPS:

- `/root/.local/state/wu-memory-benchmark/20260919-welcome/`
- `/root/.local/state/wu-memory-benchmark/20260919-project/`

Each contains `results.json` and a directory for each editor/run. Short setup checks live in separate `smoke-*` directories and are excluded from the reported results. Downloaded archives remain in `/root/.cache/wu-memory-benchmark/releases/`.

## Interpretation limits

Software rendering contributes to the measured process memory, so these absolute numbers and percentage differences should not be generalized to a hardware-GPU desktop or to macOS. The original idle scenarios measure base overhead. The active suites exercise selected editing, language-server, search, and terminal operations, but do not represent every project size or language, signed-in AI agents, debugging sessions, or hours of editing. They are memory tests, not latency or frame-rate benchmarks.

The VPS has no desktop keyring service; Zed logs credential-lookup failures for its optional account integrations. Both editors render successfully. This compares the latest released products, not builds from an identical upstream revision, and does not isolate the effect of removing any particular feature.
