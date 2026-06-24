# Dimmy Windows installer (Setup.exe) return codes

Dimmy's Windows installer (`Dimmy-win-Setup.exe`) is built with
[Velopack](https://velopack.io/). It follows the standard Windows
custom-action convention for exit codes:

| Return code | Meaning                                |
|-------------|----------------------------------------|
| `0`         | Installation successful                |
| non-zero    | Installation failed (generic failure)  |

The installer does **not** emit distinct return codes for individual
failure scenarios (user cancellation, application already present, disk
full, reboot required, network failure, security-policy rejection, etc.).
Any unsuccessful run returns a non-zero exit code and writes the details
to the Velopack log.

## Command line

| Operation        | Command                                            |
|------------------|----------------------------------------------------|
| Silent install   | `Dimmy-win-Setup.exe --silent`                     |
| Silent uninstall | `%LocalAppData%\Dimmy\Update.exe uninstall -s`     |
| Log file         | `%LocalAppData%\Dimmy\Velopack.log` (or `--log`)   |

The installer is fully offline (the application is bundled inside
`Setup.exe`) and installs per-user to `%LocalAppData%\Dimmy`. It does not
install any drivers or NT services.
