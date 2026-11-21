---
title: Understand your disk
---

<div class="hero" markdown="1">

<div class="hero-copy" markdown="1">

<p class="eyebrow"><span class="status-dot"></span> DISK EXPLORER · DESKTOP + CLI</p>

# Less disk mystery.<br>More room for<br><span class="accent-text">what matters.</span>

See what’s filling your drive. Find the files you meant to find.
Know what changed. Kosha brings a visual disk explorer, plain-language
search, and a scriptable CLI into one fast, local toolkit.
{ .hero-description }

[Download Kosha ↗](https://github.com/coolnew/kosha/releases/latest){ .button .primary }
[Take a look ↓](#a-clearer-picture-in-one-scan){ .button .secondary }

Windows · Linux · Experimental macOS
{ .platforms }

macOS: drag the DMG’s app to Applications. Not Apple-notarized; a trusted
download may need **Privacy & Security → Open Anyway** after trying to open it.
[macOS setup and warning details](../docs/manual/macos.md).
{ .hero-assurance }

Open source · Read-only scanning · No model required
{ .hero-assurance }

</div>

<div class="hero-preview" markdown="1">

<div class="window-label"><span></span><span></span><span></span><span class="window-title">Your disk, understood.</span></div>

![Kosha desktop with disk usage treemap, file list and ranked findings](../docs/manual/images/gui-overview-dark.png)

<div class="preview-caption"><span class="status-dot"></span> One scan. A map, searchable files, and useful findings.</div>

</div>

</div>

<div class="value-strip" markdown="1">

**A map for your files.** A memory for your disk. A CLI for your workflow.

[Explore the manual →](../docs/manual/README.md)

</div>

<section class="home-section" markdown="1">

<p class="eyebrow">FROM “WHERE DID IT GO?” TO “THERE IT IS.”</p>

## A clearer picture, in one scan.

Big files are only part of the story. See the space they occupy, understand
the findings, and narrow the view to what matters to you.
{ .section-intro }

<div class="feature-grid" markdown="1">

<div class="feature-card" markdown="1">

<span class="feature-number">01 / EXPLORE</span>

### Follow the space.

Go from a whole drive to a single folder in the treemap. Review ranked
findings beside your files, with explanations to guide the next step.

[Meet the desktop →](../docs/manual/desktop-tour.md)

</div>
<div class="feature-card" markdown="1">

<span class="feature-number">02 / FIND</span>

### Search like you think.

“Images larger than 10 MB.” “Files created last week.” See the interpretation,
edit the query, and export the matching paths as TXT, CSV, or JSONL.

[Explore smart search →](../docs/manual/results-and-search.md)

</div>
<div class="feature-card" markdown="1">

<span class="feature-number">03 / COMPARE</span>

### Remember what changed.

Keep snapshots and compare them to see where space grew. Return to a saved
drive scan with its age visible, then rescan when you want a fresh view.

[Work with snapshots →](../docs/manual/snapshots.md)

</div>

</div>

</section>

<section class="workflow-panel" markdown="1">

<div markdown="1">

<p class="eyebrow">YOUR FILES. YOUR CALL.</p>

## Start with understanding.

Scanning does not intentionally rewrite scanned file contents. In the desktop
app, cleanup needs an explicit mode change and review. CLI exports, snapshots
and marketplace management can still create, replace or remove files.

[How Kosha protects your files →](../docs/manual/safety.md)

</div>
<ol class="workflow-steps">
<li><span>1</span><div><strong>Scan a drive or folder</strong><p>Build a picture of what’s there.</p></div></li>
<li><span>2</span><div><strong>Explore, search, compare</strong><p>Find the files behind the numbers.</p></div></li>
<li><span>3</span><div><strong>Decide what to keep</strong><p>Review findings before taking action.</p></div></li>
</ol>

</section>

<section class="home-section" markdown="1">

<div class="section-heading" markdown="1">

<div markdown="1">

<p class="eyebrow">BUILT FOR REAL DRIVES</p>

## Millions of entries.<br>Seconds to perspective.

</div>

[Performance details and hardware →](../docs/manual/performance.md)

</div>

<div class="performance-grid" markdown="1">

<div class="performance-card" markdown="1">

<p class="performance-time">11.4<span>seconds</span></p>

**2.86 million entries · NVMe SSD**

Crucial P3 1 TB · Windows NTFS MFT · Administrator

</div>
<div class="performance-card" markdown="1">

<p class="performance-time">20.1<span>seconds</span></p>

**4.09 million entries · HDD**

Ultrastar DC HC550 16 TB · Windows NTFS MFT · Administrator

</div>

</div>

Reference observations from Kosha 0.10.3, **not a controlled benchmark**.
Cache state and repetitions were not recorded. These are filesystem metadata
scan times; results vary. NTFS MFT scanning requires administrator access.
{ .fine-print }

</section>

<section class="cli-section" markdown="1">

<div class="cli-copy" markdown="1">

<p class="eyebrow">FOR HUMANS. FOR SCRIPTS. FOR AGENTS.</p>

## Your terminal.<br>Your workflow.

Scan, search, compare snapshots, and export structured results. Use the same
toolkit interactively or give your coding agent a concise CLI guide.

[Explore the CLI →](../docs/manual/command-line.md){ .button .primary }
[Read the agent guide](../LLMS.md){ .button .secondary }

Using an agent? [Understand its access and privacy boundaries →](../docs/manual/agent-privacy.md)

</div>
<div class="cli-preview" markdown="1">

![Kosha CLI showing disk analysis and structured output for automation](../docs/manual/images/cli-agent-dark.png)

</div>

</section>

<section class="support-panel" id="agent-privacy" aria-labelledby="agent-privacy-heading" markdown="1">

<div markdown="1">

<p class="eyebrow">AGENT ACCESS &amp; PRIVACY</p>

<h2 id="agent-privacy-heading">Choose what your agent can see.</h2>

Kosha runs with the access granted to its process. Read-only scanning still
reveals metadata; Kosha does not sandbox an agent running under your account.
Contain the agent and its tools, and share only the workspace or recorded
metadata you intend it to inspect.

[Read the agent privacy guide →](../docs/manual/agent-privacy.md){ .button .primary }

</div>
<div class="support-note" markdown="1">

**Available today:** search a deliberately shared snapshot without rescanning
its original folder. The entire snapshot remains readable to its recipient.

**Starting with 0.12.0:** owner-published metadata views and
fixed-view automation sessions. [See commands and limits →](../docs/manual/metadata-views.md)
These are cooperative metadata tools, not agent sandboxes. Earlier binaries
do not include them.

</div>

</section>

<section class="recipe-panel" markdown="1">

<div markdown="1">

<p class="eyebrow">MAKE IT SPEAK YOUR LANGUAGE</p>

## A search toolkit that grows with you.

Install optional marketplace packs or author your own phrases, typed templates,
and extraction rules. Translation runs locally without an ML model. Inspect
the query before relying on the results.

[Discover the marketplace ↗](https://github.com/coolnew/kosha-marketplace){ .button .secondary }
[Create a recipe →](../docs/manual/recipe-authoring.md)

</div>
<div class="recipe-examples" aria-label="Example search phrases">
<span>files created last week</span>
<span>images larger than 10 MB</span>
<span>Your phrase → a query you can inspect</span>
</div>

</section>

<section class="support-panel" markdown="1">

<div markdown="1">

<p class="eyebrow">BUILT WITH YOUR FEEDBACK</p>

## Help shape Kosha.

Report something that does not work, or suggest an improvement. A short guided
form helps you share the details needed to investigate.

[Report a bug ↗](https://github.com/coolnew/kosha/issues/new?template=bug_report.yml){ .button .primary }
[Suggest a feature ↗](https://github.com/coolnew/kosha/issues/new?template=feature_request.yml){ .button .secondary }
[Browse existing issues](https://github.com/coolnew/kosha/issues)

</div>
<div class="support-note" markdown="1">

GitHub sign-in is required to submit. Reports are public: remove personal paths,
file names and sensitive logs before sharing. Please report security concerns
through our [private security contact](https://github.com/coolnew/kosha/security/policy).

</div>

</section>

<section class="download-panel" markdown="1">

<p class="eyebrow">MAKE SPACE FOR WHAT’S NEXT</p>

## Get to know your disk.

A visual explorer when you want to look around. A CLI when you want to get things done.

[Download Kosha ↗](https://github.com/coolnew/kosha/releases/latest){ .button .primary }
[Installation guide](../docs/manual/getting-started.md){ .button .secondary }

Windows · Linux · Experimental macOS · MIT OR Apache-2.0
{ .platforms }

</section>

<div class="trust-links" markdown="1">

[Safety and privacy](../docs/manual/safety.md) ·
[Agent access and privacy](../docs/manual/agent-privacy.md) ·
[Tests and quality](../docs/manual/quality-and-coverage.md) ·
[Measured coverage](https://coolnew.github.io/kosha/coverage/) ·
[Source on GitHub](https://github.com/coolnew/kosha)

Under active development. The manual follows current source; check your installed
version. Built with assistance from large language models, with maintainer review
and testing.
{ .fine-print }

</div>
