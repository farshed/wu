# Wu vs. Zed vs. VS Code: memory benchmarks

Wu used less settled process memory than Zed and VS Code in every measured stage of this comparison:

1. **Zed:** 9.0–10.0% less at idle and 6.7–58.2% less in the workspace workflow.
2. **VS Code:** 48% less at idle and 14.5–38.6% less in the workspace workflow.

Tested on September 26, 2026, using the official Linux ARM64 releases of [Wu 1.0.10](https://github.com/farshed/wu/releases/tag/v1.0.10) (`95d3219`), [Zed 1.21.0](https://github.com/zed-industries/zed/releases/tag/v1.21.0) (`33c9585`), and [VS Code 1.139.1](https://code.visualstudio.com/updates/v1_139) (`04c0d99`).

> These are software-rendered Linux VPS results. All editors ran real graphical interfaces through Xvfb and Mesa llvmpipe, which renders on the CPU. The numbers are not estimates of memory use on a hardware-GPU desktop.

## Results

All values are **MiB of total process-tree PSS**, where lower is better. PSS accounts proportionally for shared memory instead of counting shared pages repeatedly. Totals include the editor and its running child processes, including language servers, terminal shells, and compilers.

Each value is the median of three independent runs' settled-memory medians. Percentage reductions are relative to the named editor and calculated before rounding. Active stages run sequentially within each suite, so later rows include memory retained from earlier actions—not just the cost of that individual feature.

### Idle

| Scenario | VS Code | Zed | Wu | Less than VS Code | Less than Zed |
| --- | ---: | ---: | ---: | ---: | ---: |
| Welcome window | 743.5 | 420.4 | 382.5 | 48.5% | 9.0% |
| Repository open, no source file open | 725.2 | 423.7 | 381.2 | 47.4% | 10.0% |

### Workspace workflow

A snapshot of Wu's source repository, with language servers disabled to measure editor operations separately from language-server work.

| Stage | VS Code | Zed | Wu | Less than VS Code | Less than Zed |
| --- | ---: | ---: | ---: | ---: | ---: |
| Open 20 source files | 826.2 | 554.3 | 516.9 | 37.4% | 6.7% |
| Edit, save, and scroll | 797.6 | 557.2 | 518.8 | 35.0% | 6.9% |
| Search the repository | 823.6 | 1395.3 | 582.9 | 29.2% | 58.2% |
| Navigate and search a large log | 1326.3 | 1633.9 | 832.7 | 37.2% | 49.0% |
| Generate terminal output | 1405.5 | 1664.4 | 862.8 | 38.6% | 48.2% |
| Close tabs, retaining project state | 1007.1 | 1631.0 | 860.9 | 14.5% | 47.2% |

The workflow performs 20 verified edit/save cycles, three targeted repository searches, navigation and search in a 200,000-line log (21.9 MiB), and 20,000 lines of integrated-terminal output. The terminal retains 10,000 lines of scrollback.

## How we measured

- **Host:** Ubuntu 26.04 ARM64, six Neoverse-N1 virtual CPUs, 7.7 GiB RAM, and a 1280 × 800 virtual display. No hardware GPU.
- **Isolation:** fresh profiles and project copies for active runs; only one editor runs at a time. Wu/Zed order alternated; VS Code was measured separately. Automatic updates and telemetry are disabled, and no accounts are signed in.
- **Sampling:** Linux `/proc/PID/smaps_rollup` across each editor's process tree. Active runs wait ten seconds initially, then sample workflows at a nominal 0.5-second interval, followed by a five-second settling period and six final samples per stage. Idle runs settle for 30 seconds before ten samples at one-second intervals.
- **Coverage:** 9 active editor sessions, 54 measured stages, and 2,435 samples, plus 18 idle sessions and 180 samples. All measured editor processes had zero swap PSS.
- **Validation:** matching input-file hashes across fresh fixtures, saved-edit checks, successful terminal completion, per-process accounting checks, and rendered-window screenshots.

The totals exclude the display server, benchmark harness, shared desktop services, unmapped filesystem cache, and kernel allocations. They measure editor process memory, not the entire desktop's RAM use. Filesystem caches were not flushed.

## Scope and limitations

Software-rendering allocations contribute to these results. Hardware-GPU desktops, other operating systems, larger dependency graphs, signed-in AI features, debugging, and long editing sessions may behave differently.

## Reproduce and inspect

The [benchmark runner](../script/memory-benchmark) supports idle and workspace on Linux. It requires Python 3.11+, Xvfb, `xauth`, `dbus-run-session`, `xdotool`, `xwininfo`, `xclip`, ImageMagick, and a working Vulkan driver.
