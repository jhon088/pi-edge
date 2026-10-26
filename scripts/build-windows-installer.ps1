<#
.SYNOPSIS
    Build PX Kiosk as a Windows x64 installer.

.DESCRIPTION
    Three things, in the order they depend on each other:

      1. build the PI.Edge binaries the kiosk ships with
      2. stage them where Tauri's `externalBin` looks for them
      3. let Tauri build the NSIS installer

    Nothing here is a second build system: step 3 is the official Tauri
    pipeline, and steps 1 and 2 exist only because the installer carries the
    runtime beside the app.

    Must run on Windows: the installer links MSVC and packages a Windows
    WebView2 runtime, neither of which cross-compiles from macOS.

.PARAMETER SkipFrontend
    Reuse an existing `dist/`. Only useful when iterating on the Rust side.
#>
[CmdletBinding()]
param(
    [switch] $SkipFrontend
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Target = 'x86_64-pc-windows-msvc'
$Root = Split-Path -Parent $PSScriptRoot
$Tauri = Join-Path $Root 'apps/tauri-pos'
$Binaries = Join-Path $Tauri 'src-tauri/binaries'

function Step([string] $Message) {
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Require([string] $Command, [string] $Hint) {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) {
        throw "$Command is not on PATH. $Hint"
    }
}

Require 'cargo' 'Install Rust from https://rustup.rs.'
Require 'pnpm'  'Install Node and then `npm i -g pnpm`.'

Step "building PI.Edge for $Target"
Push-Location $Root
try {
    cargo build --release --target $Target -p edge-daemon -p edge-cli
    if ($LASTEXITCODE -ne 0) { throw 'the PI.Edge build failed' }
}
finally { Pop-Location }

Step 'staging the runtime beside the app'
New-Item -ItemType Directory -Force -Path $Binaries | Out-Null
foreach ($name in 'edge-daemon', 'edge-cli') {
    $from = Join-Path $Root "target/$Target/release/$name.exe"
    if (-not (Test-Path $from)) { throw "missing $from" }
    # Tauri resolves `externalBin` by appending the target triple.
    Copy-Item $from (Join-Path $Binaries "$name-$Target.exe") -Force
    Write-Host "    $name-$Target.exe"
}

# Tauri resolves `externalBin` by appending the target triple, and fails the
# build with "resource path doesn't exist" if either is absent. Assert it here
# instead, where the message can say which one and why.
foreach ($name in 'edge-daemon', 'edge-cli') {
    $staged = Join-Path $Binaries "$name-$Target.exe"
    if (-not (Test-Path $staged)) { throw "staging failed: $staged" }
}

Push-Location $Tauri
try {
    if (-not $SkipFrontend) {
        Step 'installing frontend dependencies'
        pnpm install --frozen-lockfile
        if ($LASTEXITCODE -ne 0) { throw 'pnpm install failed' }
    }

    Step 'building the installer'
    # `tauri build` runs `pnpm build` itself via beforeBuildCommand.
    pnpm tauri build --target $Target
    if ($LASTEXITCODE -ne 0) { throw 'the Tauri build failed' }
}
finally { Pop-Location }

$NsisDir = Join-Path $Root "target/$Target/release/bundle/nsis"
$Installer = Get-ChildItem -Path $NsisDir -Filter '*-setup.exe' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $Installer) { throw "no installer was produced in $NsisDir" }

# One predictable name, whatever Tauri called it.
$Final = Join-Path $NsisDir 'PX-Kiosk-Setup-x64.exe'
Copy-Item $Installer.FullName $Final -Force

$Size = [math]::Round((Get-Item $Final).Length / 1MB, 1)
Step "done: $Final ($Size MB)"
Write-Output $Final
