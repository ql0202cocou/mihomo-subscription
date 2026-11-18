# picomatch

The runtime files and MIT license come from the `picomatch@4.0.7` npm package.
The local version uses the parser and matcher changes from
[commit 0a8fd45](https://github.com/micromatch/picomatch/commit/0a8fd459730fe42bcddc9cee69a56e19cc5f53de)
to compile a repeated wildcard branch once when matching paths. Independent
groups elsewhere in a path, such as a brace-delimited list of extensions,
can still match alongside that branch.

The frontend uses the same copy for Vite, tinyglobby and fdir. When a
published version includes the equivalent change, remove the local dependency
and override in `web/package.json`, the frontend Docker COPY and this directory,
then regenerate the frontend lockfile.
