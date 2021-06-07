use std::{collections::BTreeMap, ffi::OsStr, io::Read, path::PathBuf};

use anyhow::{ensure, Context};
use structopt::StructOpt;

const COLORS: &[&str] = &[
    "#e6194b", "#3cb44b", "#ffe119", "#4363d8", "#f58231", "#911eb4", "#46f0f0", "#f032e6",
    "#bcf60c", "#fabebe", "#008080", "#e6beff", "#9a6324", "#fffac8", "#800000", "#aaffc3",
    "#808000", "#ffd8b1", "#000075", "#808080", "#ffffff", "#000000",
];

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct Config {
    tarballs: BTreeMap<String, String>,
}

#[derive(Debug, structopt::StructOpt)]
struct Opt {
    config: PathBuf,

    /// Use a colored graph instead of a clustered graph in the output
    #[structopt(long)]
    use_colors: bool,

    /// Return a non-zero value if some lints notice errors
    #[structopt(long)]
    lint: bool,
}

enum Publish {
    Nowhere,
    Default,
    At(Vec<String>),
}

struct Dependency {
    name: String,
    has_path: bool,
    from: Option<String>,
}

struct CrateInfo {
    name: String,
    published_to: Publish,
    deps: Vec<Dependency>,
}

fn handle_tarball(
    client: &reqwest::blocking::Client,
    dir: &tempfile::TempDir,
    name: &str,
    url: &str,
) -> anyhow::Result<Vec<CrateInfo>> {
    let url_display = if url.len() <= 40 {
        format!("{}", url)
    } else {
        format!("…{}", &url[url.len() - 39..])
    };

    // Prepare the progress bar
    let bar = indicatif::ProgressBar::new(0);
    bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] {prefix}"),
    );

    // Figure out the size of the download
    // TODO: It looks like this significantly slows down the process. Also, trying to use HEAD
    // instead of GET is even slower. Let's not have a pretty progress bar for now, it's probably
    // not a big deal anyway.
    /*
    bar.set_prefix(&format!("figuring out the size of {}", url_display));
    let r = client
        .get(url)
        .send()
        .with_context(|| format!("Failed to send HEAD request to URL {:?}", url))?;
    anyhow::ensure!(
        r.status().is_success(),
        "HEAD request to {:?} was unsuccessful",
        url
    );
    if let Some(l) = r.content_length() {
        bar.inc_length(l);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes:<8}/{total_bytes:8} ({eta}) {prefix}")
                .progress_chars("=>-"),
        );
    }
    std::mem::drop(r);
    */

    // Prepare the (compressed) archive file
    let path = dir.path().join(name);
    let dest = std::fs::File::create(&path)
        .with_context(|| format!("Failed to create file {:?}", path))?;

    // Download to it
    bar.set_prefix(&format!("downloading {}", url_display));
    let mut download = client
        .get(url)
        .send()
        .with_context(|| format!("Failed to send GET request to URL {:?}", url))?;
    download
        .copy_to(&mut bar.wrap_write(dest))
        .with_context(|| format!("Failed to download {:?} to {:?}", url, path))?;

    // Open the file, uncompressing if necessary
    let kind = infer::get_from_path(&path)
        .with_context(|| format!("Failed to read the file at {:?}", path))?;
    let archive = std::fs::File::open(&path)
        .with_context(|| format!("Failed to open the file at {:?} for reading", path))?;
    let archive: Box<dyn Read> = match kind {
        Some(t) if t.mime_type() == "application/gzip" => {
            Box::new(flate2::read::GzDecoder::new(archive)) as _
        }
        _ => Box::new(archive) as _,
    };

    // Parse tarball
    bar.set_prefix(&format!("parsing {}", url_display));
    let mut archive = tar::Archive::new(archive);

    // Iterate through the files, looking for Cargo.toml's
    let mut res = Vec::new();
    for file in archive
        .entries()
        .context("Failed to enumerate the entries of downloaded tarball")?
    {
        let mut file = file
            .context("Failed to retrieve information about an entry of the downloaded tarball")?;
        let path = file
            .path()
            .context("Failed to retrieve the path for an entry of the downloaded tarball")?;
        let path = PathBuf::from(path);
        if path.file_name() == Some(OsStr::new("Cargo.toml")) {
            // Parse the manifest
            let mut manifest = Vec::new();
            file.read_to_end(&mut manifest).with_context(|| {
                format!("Failed to read file {:?} from downloaded tarball", path)
            })?;
            let manifest = cargo_toml::Manifest::from_slice(&manifest).with_context(|| {
                format!(
                    "Failed to parse file {:?} from downloaded tarball as a Cargo.toml file",
                    path
                )
            })?;

            // Verify whether it's a virtual manifest
            let package = match manifest.package {
                Some(p) => p,
                None => continue, // Workspace Cargo.toml
            };

            // Create the dependency list
            let mut deps = Vec::new();
            for (depname, dep) in manifest
                .dependencies
                .iter()
                .chain(manifest.dev_dependencies.iter())
                .chain(manifest.build_dependencies.iter())
                .chain(manifest.target.values().flat_map(|t| {
                    t.dependencies
                        .iter()
                        .chain(t.dev_dependencies.iter())
                        .chain(t.build_dependencies.iter())
                }))
            {
                match dep {
                    cargo_toml::Dependency::Simple(_) => deps.push(Dependency {
                        name: depname.clone(),
                        has_path: false,
                        from: None,
                    }),
                    cargo_toml::Dependency::Detailed(d) => deps.push(Dependency {
                        name: d.package.clone().unwrap_or_else(|| depname.clone()),
                        has_path: d.path.is_some(),
                        from: d.registry.clone(),
                    }),
                }
            }

            // Save the crate
            res.push(CrateInfo {
                name: package.name.clone(),
                published_to: match package.publish {
                    cargo_toml::Publish::Flag(true) => Publish::Default,
                    cargo_toml::Publish::Flag(false) => Publish::Nowhere,
                    cargo_toml::Publish::Registry(registries) => Publish::At(registries),
                },
                deps,
            });
        }
    }

    bar.set_prefix(&format!("handling {}", url_display));
    bar.finish();
    Ok(res)
}

fn all_crates(
    infos: &BTreeMap<String, Vec<CrateInfo>>,
) -> impl Iterator<Item = (&str, &CrateInfo)> {
    infos
        .iter()
        .flat_map(|(k, v)| v.iter().map(move |v| (k as &str, v)))
}

fn find_info<'a>(
    name: &str,
    infos: &'a BTreeMap<String, Vec<CrateInfo>>,
) -> Option<(&'a str, &'a CrateInfo)> {
    for (repo, crates) in infos {
        for c in crates {
            if c.name == name {
                return Some((repo, c));
            }
        }
    }
    return None;
}

fn add_cycles_from(
    root_repo: &str,
    c: &CrateInfo,
    parents: &mut Vec<(String, String)>,
    infos: &BTreeMap<String, Vec<CrateInfo>>,
    cycles: &mut Vec<Vec<(String, String)>>,
) {
    for d in c.deps.iter() {
        if let Some((dep_repo, dep)) = find_info(&d.name, infos) {
            parents.push((dep_repo.to_string(), dep.name.clone()));
            if dep_repo == root_repo {
                cycles.push(parents.clone());
            } else {
                add_cycles_from(root_repo, dep, parents, infos, cycles);
            }
            parents.pop();
        }
    }
}

/// Returns true iff no lints returned any issue, false if a lint
/// returned an issue, and an error if the input was too broken to be
/// able to generate a graph
fn sanity_check(infos: &BTreeMap<String, Vec<CrateInfo>>) -> anyhow::Result<bool> {
    // Check that there are not two crates with the same name
    let mut name_to_repo = BTreeMap::new();

    for (repo, infos) in infos.iter() {
        for i in infos.iter() {
            if let Some(r) = name_to_repo.get(&i.name) {
                anyhow::bail!(
                    "Crate {} was defined multiple times, eg. in repos {} and {}",
                    i.name,
                    r,
                    repo
                );
            }
            name_to_repo.insert(i.name.clone(), repo);
        }
    }

    // Check the circular dependencies across repositories
    //
    // Naive algorithm for now, because complexity is not really
    // important with relatively few repositories: for each node in
    // the graph (root), look down the dependency tree until finding
    // one that has the same repo, while checking that the first
    // dependency was in another repo
    let mut cycles = Vec::new();
    for (root_repo, root) in all_crates(infos) {
        for d in root.deps.iter() {
            let dep_name = &d.name;
            if let Some((dep_repo, dep)) = find_info(dep_name, infos) {
                if dep_repo != root_repo {
                    add_cycles_from(
                        root_repo,
                        dep,
                        &mut vec![
                            (root_repo.to_string(), root.name.clone()),
                            (dep_repo.to_string(), dep_name.clone()),
                        ],
                        infos,
                        &mut cycles,
                    );
                }
            }
        }
    }
    if !cycles.is_empty() {
        eprintln!(
            "Cyclic dependencies across repositories ({}):",
            cycles.len()
        );
    }
    let all_lints_passed = cycles.is_empty();
    for c in cycles {
        eprint!(" *");
        for (repo, krate) in c {
            eprint!(
                " {}{}",
                console::style(krate).for_stderr().bold(),
                console::style(format!("[{}]", repo))
                    .for_stderr()
                    .dim()
                    .italic(),
            );
        }
        eprintln!();
    }

    Ok(all_lints_passed)
}

#[derive(Debug, Eq, PartialEq)]
enum GraphType {
    Cluster,
    Colors,
}

fn make_graph(
    graph_type: GraphType,
    infos: &BTreeMap<String, Vec<CrateInfo>>,
) -> anyhow::Result<()> {
    println!("digraph G {{");
    println!("    node [shape=rectangle]");

    // First, put all the nodes in their repository
    if graph_type == GraphType::Cluster {
        for (repo, infos) in infos.iter() {
            println!("    subgraph \"cluster_{}\" {{", repo);
            println!("        label = \"{}\";", repo);
            println!("        style = filled;");
            for i in infos.iter() {
                let color = match i.published_to {
                    Publish::Nowhere => "color=blue",
                    Publish::Default => "color=green",
                    Publish::At(_) => "",
                };
                println!("        \"{}\" [{}];", i.name, color);
            }
            println!("    }}");
        }
    } else {
        ensure!(infos.len() <= COLORS.len(), "asked for a color-based output while there are more repositories than colors available");
        for (idx, (_, infos)) in infos.iter().enumerate() {
            for i in infos.iter() {
                println!(
                    "    \"{}\" [style=filled, fillcolor=\"{}\"];",
                    i.name, COLORS[idx]
                );
            }
        }
    }

    // Then, draw all arrows
    for (_, infos) in infos.iter() {
        for i in infos.iter() {
            for d in i.deps.iter() {
                // For now we're interested only in stuff from our own registry or that has
                // path-local dependencies
                if d.from.is_some() || d.has_path {
                    let color = if d.has_path { "[color=blue]" } else { "" };
                    println!("    \"{}\" -> \"{}\" {};", i.name, d.name, color);
                }
            }
        }
    }

    println!("}}");

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let opt = Opt::from_args();

    let cfg =
        std::fs::read(&opt.config).with_context(|| format!("Failed to read {:?}", opt.config))?;
    let cfg: Config =
        toml::from_slice(&cfg).with_context(|| format!("Failed to parse {:?}", opt.config))?;

    let dir = tempfile::tempdir().context("Failed to create a temporary directory")?;

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("Failed to initialize reqwest")?;

    let infos: BTreeMap<String, Vec<CrateInfo>> = cfg
        .tarballs
        .iter()
        .map(|(name, url)| -> anyhow::Result<(String, Vec<CrateInfo>)> {
            Ok((
                name.clone(),
                handle_tarball(&client, &dir, name, url).with_context(|| {
                    format!("Failed to retrieve informations for repository {}", name)
                })?,
            ))
        })
        .collect::<anyhow::Result<_>>()?;

    let all_lints_passed =
        sanity_check(&infos).context("Failed to sanity-check the computed information")?;

    let graph_type = match opt.use_colors {
        true => GraphType::Colors,
        false => GraphType::Cluster,
    };
    make_graph(graph_type, &infos).context("Failed to output the dependency graph")?;

    if opt.lint {
        anyhow::ensure!(
            all_lints_passed,
            "Some lints reported issues, see error log above"
        );
    }

    Ok(())
}
