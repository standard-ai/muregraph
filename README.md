# muregraph

`muregraph` is a tool that graphs Rust dependencies across multiple repositories
(**mu**ltiple **re**pository **graph**), thus making it easy to identify which
repositories depend on which other repositories.

## Usage

`muregraph` outputs graphviz description language. As such, you can pipe it into
whatever graphviz tool you want to generate pretty graphs.

In order to run it, you must be able to download tarballs for the repositories
that are of interest to you. In order to do that for private repositories (as
suggested by the `config.toml.example`), you will need a GitHub personal token,
that you can generate [here](https://github.com/settings/tokens).

An example usage would be:
```bash
$ cp config.toml{.example,}
$ sed -i 's/GITHUB_TOKEN/[your github token]/' config.toml
$ muregraph config.toml | fdp -Txlib
```

Note that `zgrviewer` is a great way to visualize the graph, as it can quickly
become quite entangled.

## Lints

`muregraph` takes advantage of the fact that it generates the crate graph to
provide some lints, that get shown on standard error. In order to have
`muregraph` return an error upon a failing lint, please use `--lint`.

## Description of the output

Nodes are:
- Black if they are published to a non-public registry
- Blue if they are not published to any registry
- Green if they are not tagged as being either unpublished or published to a
  non-public registry

Edges are:
- Blue if they are path-local
- Black if they go through a registry

As such, of particular interest are:
- Circular dependencies between repositories
- Green boxes, that show crates that are probably open-source
- Blue edges that point to black nodes, as this would indicate a possible
  version mismatch when changes occur
