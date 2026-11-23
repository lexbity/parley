# Install the Parley launcher

This internal Linux x86_64 archive starts Parley's local service and opens your system browser. The qualification runner uses Ubuntu 24.04.

1. Keep the archive and its accompanying `SHA256SUMS` together. Confirm the archive digest with `sha256sum <archive>.tar.gz` and compare it with the corresponding entry in `SHA256SUMS`.
2. Extract the verified archive into a new directory with `tar -xzf <archive>.tar.gz`.
3. Open the extracted directory and run `./parley`. Run `./parley --headless` to get a one-time loopback URL for manual opening. Keep that URL private.

The launcher uses `$XDG_DATA_HOME/parley/workspace.sqlite` or `$HOME/.local/share/parley/workspace.sqlite` by default. Use `--workspace /absolute/path/workspace.sqlite` to open a dedicated Workspace. Parley can start and handle manual work without a model installed.

Before upgrading, create a verified backup in **Settings**, close Parley, and preserve a copy of the entire stopped Workspace and the previous archive. Extract each software version into its own directory. Open a copied Workspace with the new executable to validate migration before upgrading the working Workspace.

To recover, close the new executable and reopen a separate pre-upgrade Workspace copy with the previous executable. Keep the upgraded Workspace available for export and review. Removing an extracted launcher directory leaves Workspace data in its configured location.
