$ErrorActionPreference = "Stop"
Push-Location (Join-Path $PSScriptRoot "..")
try {
    cargo install --path crates/repoise-cli --locked @args
    if ($LASTEXITCODE -ne 0) { throw "Install failed" }
} finally { Pop-Location }
