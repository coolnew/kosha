// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::path::Path;

use kosha_core::sayln;
use kosha_marketplace::catalog;

const HELP: &str = "kosha marketplace sync\n\
                   kosha marketplace search [TERMS...]\n\
                   kosha marketplace show PACK-ID\n\
                   kosha marketplace get PACK-ID\n\
                   kosha marketplace install FILE\n\
                   kosha marketplace list\n\
                   kosha marketplace uninstall PACK-ID\n\n\
                   Sync the public catalog, then search it offline or use get to download and install a pack.\n\
                   Use publisher/id when IDs overlap. Unsupported package kinds and capabilities are browse-only.\n\
                   Use install for a local JSON file.\n\
                   The CLI and GUI share installed packs. Personal search-recipes.json stays separate.";

pub fn run(args: &[String]) -> Result<(), String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["--help" | "-h"] => sayln!("{HELP}"),
        ["sync"] => {
            let catalog = catalog::sync()?;
            sayln!("Synced marketplace revision {}", catalog.revision);
        }
        ["search", terms @ ..] => {
            let catalog = catalog::cached()?;
            let mut entries = catalog.search(&terms.join(" ")).peekable();
            if entries.peek().is_none() {
                sayln!("No marketplace packages match this search.");
            }
            for entry in entries {
                let pack = &entry.metadata;
                sayln!("{}  {}  {}", pack.qualified_id(), pack.version, pack.title);
                if let Some(reason) = entry.unavailable_reason() {
                    sayln!("  Unavailable: {reason}");
                }
            }
        }
        ["show", id] => {
            let catalog = catalog::cached()?;
            let entry = catalog.find(id)?;
            let pack = &entry.metadata;
            let platforms = if entry.platforms.is_empty() {
                "all".into()
            } else {
                entry.platforms.join(", ")
            };
            sayln!(
                "{} ({}, version {})",
                pack.title,
                pack.qualified_id(),
                pack.version
            );
            sayln!("{}", pack.description);
            sayln!("Kind: {}", entry.kind);
            if let Some(reason) = entry.unavailable_reason() {
                sayln!("Unavailable: {reason}");
            }
            sayln!("Platforms: {platforms}");
            sayln!("Requires: {}", entry.requires.join(", "));
            sayln!("Tags: {}", pack.tags.join(", "));
            sayln!("Catalog revision: {}", catalog.revision);
        }
        ["get", id] => {
            print_installed(catalog::install(id)?);
        }
        ["install", file] => {
            print_installed(kosha_marketplace::install_local(Path::new(file))?);
        }
        ["list"] => {
            let packs = kosha_marketplace::installed()?;
            if packs.is_empty() {
                sayln!("No installed recipe packs.");
            }
            for pack in packs {
                sayln!("{}  {}  {}", pack.qualified_id(), pack.version, pack.title);
            }
        }
        ["uninstall", id] => {
            kosha_marketplace::uninstall(id)?;
            sayln!("Uninstalled {id}");
        }
        _ => return Err(format!("Invalid marketplace command\n{HELP}")),
    }
    Ok(())
}

fn print_installed(pack: kosha_search::RecipePackMetadata) {
    sayln!(
        "Installed {} ({}, version {})",
        pack.title,
        pack.qualified_id(),
        pack.version
    );
}
