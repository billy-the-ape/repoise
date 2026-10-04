$ErrorActionPreference = "Stop"
Push-Location (Join-Path $PSScriptRoot "..")
try {
    cargo build --release --package repoise-cli --locked
    if ($LASTEXITCODE -ne 0) { throw "Build failed" }
} finally { Pop-Location }
