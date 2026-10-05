<#
.SYNOPSIS
Starts a local Mirrorwarfare FFA match on a Catalyst arena with bots.

.EXAMPLE
.\Mirrorwarfare.cmd                       # random arena, 7 bots
.\Mirrorwarfare.cmd -Arena mec_anchor_1 -Bots 11
.\Mirrorwarfare.cmd -Menu                 # main menu; Private Match lists the arenas under CATALYST
.\Mirrorwarfare.cmd -List
#>
[CmdletBinding()]
param(
    # Arena name (mec_anchor_1), or "random".
    [string]$Arena = "random",
    [ValidateRange(0, 16)][int]$Bots = 7,
    # Spawn straight into the first class instead of choosing one.
    [switch]$Spawn,
    # Open the main menu instead of starting a match.
    [switch]$Menu,
    [switch]$List,
    # Opt out of sound (it is on by default).
    [switch]$NoSound,
    # iw4l.exe to run. Default: IW4L_EXE, else this repo's target\play (then target\release).
    [string]$Exe,
    # Extra console commands appended to the match script.
    [string]$Cmds
)

$ErrorActionPreference = "Stop"
$RepoDir = $PSScriptRoot
$WorkRoot = Split-Path $RepoDir -Parent

function Find-Exe {
    if ($Exe) { return (Resolve-Path $Exe).Path }
    if ($env:IW4L_EXE -and (Test-Path $env:IW4L_EXE)) { return (Resolve-Path $env:IW4L_EXE).Path }
    # The repo's own build only: side build dirs (..\iw4l-target-*) hold work in progress.
    foreach ($c in @((Join-Path $RepoDir "target\play\iw4l.exe"), (Join-Path $RepoDir "target\release\iw4l.exe"))) {
        if (Test-Path $c) { return (Resolve-Path $c).Path }
    }
    return $null
}

# Same precedence as the game (asset_mec::arenas_root).
function Find-Arenas([string]$exeDir) {
    if ($env:IW4L_MEC_ARENAS) { return $env:IW4L_MEC_ARENAS }
    $artifacts = if ($env:IW4L_ARTIFACTS_DIR) { $env:IW4L_ARTIFACTS_DIR } else { Join-Path $exeDir "iw4l-artifacts" }
    $dirs = @(Join-Path $artifacts "mec-arenas")
    foreach ($start in @($exeDir, $RepoDir)) {
        $dir = $start
        while ($dir) {
            $dirs += Join-Path $dir "mec-arenas"
            $dir = Split-Path $dir -Parent
        }
    }
    foreach ($dir in $dirs) { if (Test-Path $dir -PathType Container) { return $dir } }
    return $null
}

function Get-ArenaNames([string]$root) {
    @(Get-ChildItem -Path $root -Directory -ErrorAction SilentlyContinue |
        Where-Object { -not $_.Name.StartsWith("_") -and (Test-Path (Join-Path $_.FullName "arena.json")) -and (Test-Path (Join-Path $_.FullName "arena.glb")) } |
        ForEach-Object { $_.Name } | Sort-Object)
}

$exePath = Find-Exe
if (-not $exePath) {
    Write-Host "iw4l.exe not found. Build it first:" -ForegroundColor Red
    Write-Host "  cargo build --profile play -p launcher"
    exit 1
}
$exeDir = Split-Path $exePath -Parent

$arenaRoot = Find-Arenas $exeDir
$names = if ($arenaRoot) { Get-ArenaNames $arenaRoot } else { @() }
if ($names.Count -eq 0) {
    Write-Host "No Catalyst arenas found (looked for mec-arenas beside $exeDir and its parents)." -ForegroundColor Red
    Write-Host "  Build them:  powershell -ExecutionPolicy Bypass -File tools\mec\setup.ps1"
    Write-Host "  or point IW4L_MEC_ARENAS at an arena folder."
    exit 1
}
if ($List) {
    Write-Host "arenas in ${arenaRoot}:"
    $names | ForEach-Object { Write-Host "  $_" }
    exit 0
}

if ($Arena -eq "random") {
    $pick = @($names | Where-Object { $_ -ne "testbox" })
    if ($pick.Count -eq 0) { $pick = $names }
    $Arena = $pick | Get-Random
} elseif ($Arena -notin $names) {
    Write-Host "Arena `"$Arena`" is not in $arenaRoot. Available: $($names -join ', ')" -ForegroundColor Red
    exit 1
}

$env:IW4L_MEC_ARENAS = $arenaRoot
if (-not $env:IW4L_GAMETYPE) { $env:IW4L_GAMETYPE = "dm" }
# Sound is on by default (the arena composes its bank from the donor map's
# zone); -NoSound is only an opt-out, and a stale IW4L_SOUND=off from an
# earlier session does not silently carry over.
if ($NoSound) { $env:IW4L_SOUND = "off" } elseif ($env:IW4L_SOUND -in @("off", "0")) { Remove-Item Env:IW4L_SOUND }
if (-not $env:IW4L_GAMES -and (Test-Path (Join-Path $RepoDir ".env"))) {
    $line = Select-String -Path (Join-Path $RepoDir ".env") -Pattern '^\s*IW4L_GAMES\s*=\s*"?([^"]+)"?\s*$' | Select-Object -First 1
    if ($line) { $env:IW4L_GAMES = $line.Matches[0].Groups[1].Value }
}

if ($Menu) {
    $arguments = @("menu")
    $env:IW4L_BOTS = "$Bots"
    Write-Host "Mirrorwarfare menu: Private Match > Game Setup > Change Map > CATALYST ($($names.Count) arenas), Bots: $Bots"
} else {
    $script = @("wait world")
    if ($Bots -gt 0) { $script += "bot add $Bots" }
    if ($Spawn) { $script += "spawn 0" }
    if ($Cmds) { $script += $Cmds }
    $arguments = @("map", "mec:$Arena", "--cmds", ($script -join "; "))
    Write-Host "Mirrorwarfare: FFA on $Arena with $Bots bots"
}
Write-Host "  $exePath $($arguments -join ' ')" -ForegroundColor DarkGray
Write-Host "  arenas: $arenaRoot; log: $exeDir\iw4l-artifacts\logs\latest.log" -ForegroundColor DarkGray
Start-Process -FilePath $exePath -ArgumentList ($arguments | ForEach-Object { if ($_ -match '[\s;]') { "`"$_`"" } else { $_ } }) -WorkingDirectory $exeDir
