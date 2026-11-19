# Performance reference runs

These Windows observations give a sense of Kosha on real storage. They are
**reference runs, not a controlled benchmark or a performance guarantee**.
The operator supplied one summary for each case and identified the version as
**Kosha 0.10.3**. Exact commit, build profile, run dates, run order, repeat counts
and cold/warm filesystem-cache state were not recorded with these excerpts.
The observations were submitted on 2026-11-19; this is not an asserted run date.

## Windows observations

Entries include files and directories. Elapsed times are the scan durations
printed by the CLI, rather than independently measured whole-process timings.
Methods and privileges below are as reported by the operator.

| Storage | Method / privilege | Entries | Scan time | Reported coverage |
| --- | --- | ---: | ---: | --- |
| NVMe SSD, D: | Directory walk, standard user | 2,849,859 | 15.3 s | 10 unreadable entries; lower-bound totals |
| NVMe SSD, D: | NTFS MFT, administrator | 2,855,411 | 11.4 s | All files accessed, as reported |
| HDD, E: | Directory walk, standard user | 4,088,836 | 12m 32s | 22 unreadable entries; lower-bound totals |
| HDD, E: | NTFS MFT, administrator | 4,089,362 | 20.1 s | All files accessed, as reported |

### Reported sizes

| Observation | On disk | Apparent |
| --- | ---: | ---: |
| NVMe, standard user | ~614 GiB | ~608 GiB |
| NVMe, administrator / MFT | 618 GiB | 615 GiB |
| HDD, standard user | ~4.92 TiB | ~4.91 TiB |
| HDD, administrator / MFT | 4.81 TiB | 4.80 TiB |

The `~` values are lower bounds because some entries could not be read. The
MFT coverage notes come from the operator; they are not an independent audit
that every possible object on the volume was covered.

**The observations have different entry counts and size totals.** In particular,
the HDD walk reports larger byte totals than the MFT observation. The excerpts
do not establish why. Privileges, strategy, hardlink/allocation accounting,
scan scope and changes between runs can affect the result. These numbers are
preserved as supplied and do not establish a like-for-like speedup ratio or
which size total is more accurate. Read [scan scope](scanning.md) when comparing
observations.

## Hardware context

The following context was read from the same workstation when documenting the
results. It was not captured as a per-run environment log. Serial numbers,
volume labels and personal file paths are omitted.

| Volume | Identified hardware | Nominal capacity | Filesystem |
| --- | --- | ---: | --- |
| D: | Crucial P3, `CT1000P3SSD8`, NVMe SSD | 1 TB | NTFS, 4 KiB allocation units |
| E: | WD Ultrastar DC HC550, `WUH721816ALE6L4`, SATA HDD | 16 TB | NTFS, 4 KiB allocation units |

Workstation context: Intel Core i9-14900K, 24 cores / 32 logical processors;
approximately 128 GiB OS-visible memory; Windows 11 Pro, build 26300. Run-time
CPU load, memory use, background activity, drive firmware and storage-cache
configuration were not recorded.

### Manufacturer specifications

These describe the drive models, not measured raw I/O performance of this
workstation:

- **Crucial P3 1 TB:** sequential read up to 3,500 MB/s and sequential write up
  to 3,000 MB/s in the manufacturer's product flyer. Its performance conditions
  include a fresh drive state, a full command queue and enabled write caching.
  [Crucial P3 product flyer](https://www.crucial.com/content/dam/crucial/ssd-products/p3/flyers/b2c/crucial-p3-productflyer-consumer.pdf).
- **Ultrastar DC HC550 16 TB:** SATA 6 Gb/s interface, 7,200 RPM and 512 MB buffer;
  maximum sustained transfer rate of 262 MB/s (250 MiB/s) for the 16 TB model.
  [Western Digital HC550 data sheet](https://documents.westerndigital.com/content/dam/doc-library/en_us/assets/public/western-digital/product/data-center-drives/ultrastar-dc-hc500-series/data-sheet-ultrastar-dc-hc550.pdf).

Manufacturer capacities and MB/s are decimal; Kosha's GiB/TiB figures are binary.
No raw-disk throughput test was run to prepare this page.

## What the timings measure

Kosha's ordinary scan enumerates filesystem metadata. The NTFS MFT strategy
reads the filesystem's metadata table. The size totals describe recorded disk
usage; reporting 4.81 TiB in 20.1 seconds does **not** mean reading 4.81 TiB of
file contents in that time. Duplicate-content checking would read file data
and is a different workload. Neither these scan times nor the vendor's
sequential-transfer specifications establish content-read throughput.

Filesystem cache state, directory layout, fragmentation, permissions, scan
strategy and background work can change timings. These are single supplied
observations, not an average, percentile, cold-cache measurement, regression
baseline or comparison with another product.

## Supplied excerpts

NVMe, standard user:

```text
~614 GiB on disk  ~608 GiB apparent  2,849,859 entries in 15.3 s
10 entries could not be read, so totals are a lower bound — `--query unreadable` lists them
```

NVMe, administrator, command reported as `kosha D: --mft`:

```text
618 GiB on disk  615 GiB apparent  2,855,411 entries in 11.4 s
All files accessed
```

HDD, standard user:

```text
~4.92 TiB on disk  ~4.91 TiB apparent  4,088,836 entries in 12m 32s
22 entries could not be read, so totals are a lower bound — `--query unreadable` lists them
```

HDD, administrator, command reported as `kosha E: --mft`:

```text
4.81 TiB on disk  4.80 TiB apparent  4,089,362 entries in 20.1 s
All files accessed
```

The standard-user command lines and full logs were not supplied. The two
"All files accessed" lines preserve the operator's coverage notes.

## Capture a future reference run

Use explicit drive roots such as `D:\` and `E:\`: Windows `D:` and `E:` are
drive-relative paths and can depend on a remembered current directory. Capture
the actual reported scan root and strategy as well as the command. For example:

```powershell
kosha --version
kosha D:\ --stats --top 0
kosha E:\ --stats --top 0
# In a separate administrator terminal:
kosha D:\ --mft --stats --top 0
kosha E:\ --mft --stats --top 0
```

These are suggested future commands, not a claim that the supplied observations
were collected with these exact options. `--mft` can fall back when unavailable;
keep its strategy diagnostics. Record time/date, version/commit, hardware,
filesystem, relevant flags, cache state, run order, repetitions, background load,
coverage warnings and original outputs. Review paths before sharing logs.
Use an unchanged dataset and repeated, controlled runs before making comparative
or formal benchmark claims. Linux observations can be added as a separate
environment when collected.
