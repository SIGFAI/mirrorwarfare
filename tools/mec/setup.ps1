<#
.SYNOPSIS
Builds the Mirrorwarfare Catalyst arenas from your own Mirror's Edge Catalyst install.

.DESCRIPTION
Idempotent and resumable; every step skips work that is already done:
  1. uv (Python runner) is found on PATH or in %USERPROFILE%\.local\bin.
  2. Catalyst dump: if <DumpDir>\resTable.bin is missing, Catalyst is located through Steam
     (libraryfolders.vdf) or the EA app folders, Frostbite-Scripts is fetched at a pinned
     commit into <ToolsDir>, and its frostbite3 dumper extracts the game (~25 min, ~40 GB).
     An interrupted dump resumes: the dumper skips files it already wrote.
  3. Caches: <CacheDir>\ebx_index.json (~7 min) and world_instances.json (~70 s).
  4. Arenas: each missing arena (no stats.json yet) is cut from SP_MainCity (~1-3 min each)
     and checked with verify_arena.py. Finished arenas get their menu title if they lack one.
  5. Character animations: Faith's third-person parkour clips (wallrun, vault, slide, roll,
     climb, ...) are decoded from the dump's ANT AssetBank into <AnimsDir> (~2 min) by
     antanim.py; the game retargets them onto the MW2 player bodies (IW4L_MEC_ANIMS overrides
     where it looks).

Nothing is written to the game folder. Game data stays outside the repository.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File tools\mec\setup.ps1
.EXAMPLE
powershell -ExecutionPolicy Bypass -File tools\mec\setup.ps1 -Arenas mec_anchor_1 -DryRun
#>
[CmdletBinding()]
param(
    # Work root holding mec-dump, mec-arenas and mec-tools. Default: the folder containing the iw4L checkout.
    [string]$Root,
    [string]$DumpDir,
    [string]$ArenasDir,
    [string]$CacheDir,
    [string]$ToolsDir,
    # Decoded Catalyst character clips (default: <Root>\mec-anims).
    [string]$AnimsDir,
    # Catalyst install folder (the one holding Data\layout.toc). Default: found through Steam / EA app.
    [string]$CatalystDir,
    # Git URL or local clone of Frostbite-Scripts.
    [string]$DumperSource = "https://github.com/NicknineTheEagle/Frostbite-Scripts",
    # Subset of arena names to build (default: all).
    [string[]]$Arenas,
    # Never run the 25-minute dumper; stop before the cache step if the dump is missing.
    [switch]$SkipDump,
    [switch]$SkipVerify,
    # Rebuild arenas that are already complete.
    [switch]$Force,
    # Print what would run without running it.
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$DumperCommit = "a07ee6d38ecc62598dfc9fdf990be067d9f21929"
$Python = "3.13"

# name, Frostbite centre x z, edge (m), --ymin, menu title
$ArenaTable = @(
    @{ Name = "mec_anchor_1"; Center = @(345, 470); Size = 170; YMin = -10; Title = "Anchor Rooftops" },
    @{ Name = "mec_west_1"; Center = @(-130, 70); Size = 180; YMin = -10; Title = "West Tower Plaza" },
    @{ Name = "mec_north_1"; Center = @(150, 810); Size = 180; YMin = -10; Title = "North Highrises" }
)

$ToolDir = $PSScriptRoot
$RepoDir = (Resolve-Path (Join-Path $ToolDir "..\..")).Path
if (-not $Root) { $Root = Split-Path $RepoDir -Parent }
if (-not $DumpDir) { $DumpDir = Join-Path $Root "mec-dump" }
if (-not $ArenasDir) { $ArenasDir = Join-Path $Root "mec-arenas" }
if (-not $CacheDir) { $CacheDir = Join-Path $ArenasDir "_cache" }
if (-not $ToolsDir) { $ToolsDir = Join-Path $Root "mec-tools" }
if (-not $AnimsDir) { $AnimsDir = Join-Path $Root "mec-anims" }

function Step([string]$text) { Write-Host ""; Write-Host "== $text" -ForegroundColor Cyan }
function Info([string]$text) { Write-Host "   $text" }
function Fail([string]$text) { Write-Host "   $text" -ForegroundColor Red; exit 1 }

function Invoke-Tool([string]$exe, [string[]]$arguments, [string]$workDir) {
    $shown = ($arguments | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join " "
    Write-Host "   > $exe $shown" -ForegroundColor DarkGray
    if ($DryRun) { return }
    if ($workDir) { Push-Location $workDir }
    try {
        & $exe @arguments
        if ($LASTEXITCODE -ne 0) { Fail "failed with exit code $LASTEXITCODE" }
    } finally {
        if ($workDir) { Pop-Location }
    }
}

function Find-Uv {
    $cmd = Get-Command uv -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $local = Join-Path $env:USERPROFILE ".local\bin\uv.exe"
    if (Test-Path $local) { return $local }
    return $null
}

function Find-Catalyst {
    $roots = @()
    foreach ($key in @("HKCU:\Software\Valve\Steam", "HKLM:\SOFTWARE\WOW6432Node\Valve\Steam", "HKLM:\SOFTWARE\Valve\Steam")) {
        $item = Get-ItemProperty $key -ErrorAction SilentlyContinue
        if ($item -and $item.SteamPath) { $roots += $item.SteamPath }
        if ($item -and $item.InstallPath) { $roots += $item.InstallPath }
    }
    $roots += "C:\Program Files (x86)\Steam"
    $libraries = @()
    foreach ($steam in ($roots | Select-Object -Unique)) {
        $libraries += $steam
        $vdf = Join-Path $steam "steamapps\libraryfolders.vdf"
        if (Test-Path $vdf) {
            foreach ($m in [regex]::Matches((Get-Content $vdf -Raw), '"path"\s+"([^"]+)"')) {
                $libraries += $m.Groups[1].Value -replace '\\\\', '\'
            }
        }
    }
    $candidates = @($libraries | Select-Object -Unique | ForEach-Object { Join-Path $_ "steamapps\common\Mirrors Edge Catalyst" })
    $candidates += @(
        "C:\Program Files\EA Games\Mirror's Edge Catalyst",
        "C:\Program Files\EA Games\Mirrors Edge Catalyst",
        "C:\Program Files (x86)\Origin Games\Mirrors Edge Catalyst"
    )
    foreach ($dir in $candidates) {
        if (Test-Path (Join-Path $dir "Data\layout.toc")) { return $dir }
    }
    return $null
}

function Get-Dumper {
    $dir = Join-Path $ToolsDir "Frostbite-Scripts"
    $marker = Join-Path $dir ".mirrorwarfare-commit"
    if ((Test-Path $marker) -and ((Get-Content $marker -Raw).Trim() -eq $DumperCommit)) {
        Info "Frostbite-Scripts $($DumperCommit.Substring(0, 7)) at $dir"
        return $dir
    }
    if (Test-Path $dir) {
        Info "replacing incomplete $dir"
        if (-not $DryRun) { Remove-Item -Recurse -Force $dir }
    }
    if (-not $DryRun) { New-Item -ItemType Directory -Force $ToolsDir | Out-Null }
    $git = Get-Command git -ErrorAction SilentlyContinue
    if ($git) {
        Invoke-Tool $git.Source @("clone", "--quiet", $DumperSource, $dir)
        Invoke-Tool $git.Source @("-C", $dir, "-c", "advice.detachedHead=false", "checkout", "--quiet", $DumperCommit)
    } elseif ($DumperSource -match '^https?://') {
        $zip = Join-Path $ToolsDir "Frostbite-Scripts-$DumperCommit.zip"
        Info "downloading $DumperSource/archive/$DumperCommit.zip"
        if (-not $DryRun) {
            Invoke-WebRequest -UseBasicParsing "$DumperSource/archive/$DumperCommit.zip" -OutFile $zip
            Expand-Archive $zip -DestinationPath $ToolsDir -Force
            Move-Item (Join-Path $ToolsDir "Frostbite-Scripts-$DumperCommit") $dir
            Remove-Item $zip
        }
    } else {
        Fail "git is required to fetch Frostbite-Scripts from $DumperSource"
    }
    if (-not $DryRun) {
        if (-not (Test-Path (Join-Path $dir "frostbite3\dumper.py"))) { Fail "$dir has no frostbite3\dumper.py" }
        Set-Content -Path $marker -Value $DumperCommit -Encoding ascii
    }
    return $dir
}

# The upstream dumper takes its paths from two assignments at the top of dumper.py.
function Write-DumperScript([string]$dumper, [string]$game, [string]$target) {
    $source = Join-Path $dumper "frostbite3\dumper.py"
    $script = Join-Path $dumper "frostbite3\dumper_mirrorwarfare.py"
    if ($DryRun) { return $script }
    $text = [IO.File]::ReadAllText($source)
    $game = $game -replace '\\', '/'
    $target = $target -replace '\\', '/'
    $text = ([regex]'(?m)^gameDirectory\s*=\s*r".*$').Replace($text, "gameDirectory   = r`"$game`"", 1)
    $text = ([regex]'(?m)^targetDirectory\s*=\s*r".*$').Replace($text, "targetDirectory = r`"$target`"", 1)
    [IO.File]::WriteAllText($script, $text, (New-Object Text.UTF8Encoding($false)))
    return $script
}

function Test-ArenaComplete([string]$dir) {
    foreach ($file in @("arena.json", "arena.glb", "collision.glb", "stats.json")) {
        if (-not (Test-Path (Join-Path $dir $file))) { return $false }
    }
    return $true
}

Write-Host "Mirrorwarfare arena setup" -ForegroundColor Cyan
Info "work root  $Root"
Info "dump       $DumpDir"
Info "arenas     $ArenasDir"
Info "cache      $CacheDir"
if ($DryRun) { Info "dry run: nothing is written" }

Step "1/5 tools"
$uv = Find-Uv
if (-not $uv) {
    Fail "uv not found. Install it: powershell -ExecutionPolicy ByPass -c `"irm https://astral.sh/uv/install.ps1 | iex`""
}
Info "uv: $uv"

Step "2/5 Catalyst dump"
$dumpDone = (Test-Path (Join-Path $DumpDir "resTable.bin")) -and (Test-Path (Join-Path $DumpDir "guidTable.bin"))
if ($dumpDone) {
    Info "present ($DumpDir)"
} else {
    if (-not $CatalystDir) { $CatalystDir = Find-Catalyst }
    if (-not $CatalystDir -or -not (Test-Path (Join-Path $CatalystDir "Data\layout.toc"))) {
        Fail "Mirror's Edge Catalyst not found. Pass -CatalystDir <folder holding Data\layout.toc>."
    }
    Info "Catalyst: $CatalystDir"
    $dumper = Get-Dumper
    $script = Write-DumperScript $dumper $CatalystDir $DumpDir
    if ((Test-Path $DumpDir) -and -not $dumpDone) { Info "resuming the partial dump in $DumpDir" }
    if ($SkipDump) {
        Info "-SkipDump: not running the dumper. To dump, run without -SkipDump, or:"
        Info "  cd `"$(Split-Path $script)`"; & `"$uv`" run --no-project --python $Python $(Split-Path $script -Leaf)"
        if (-not $DryRun) { Fail "dump missing: $DumpDir\resTable.bin" }
    } else {
        Info "dumping (about 25 minutes; progress lists each table of contents)"
        Invoke-Tool $uv @("run", "--no-project", "--python", $Python, (Split-Path $script -Leaf)) (Split-Path $script)
        if (-not $DryRun -and -not (Test-Path (Join-Path $DumpDir "resTable.bin"))) {
            Fail "the dumper finished without writing $DumpDir\resTable.bin"
        }
    }
}

Step "3/5 asset caches"
$cacheDone = (Test-Path (Join-Path $CacheDir "ebx_index.json")) -and (Test-Path (Join-Path $CacheDir "world_instances.json"))
if ($cacheDone) {
    Info "present ($CacheDir)"
} else {
    Info "building (ebx index ~7 min, world scan ~70 s)"
    Invoke-Tool $uv @("run", "--python", $Python, (Join-Path $ToolDir "prepare_cache.py"), "--dump", $DumpDir, "--cache", $CacheDir)
}

Step "4/5 arenas"
$selected = $ArenaTable
if ($Arenas) {
    $unknown = @($Arenas | Where-Object { $_ -notin ($ArenaTable | ForEach-Object { $_.Name }) })
    if ($unknown.Count -gt 0) { Fail "unknown arena(s): $($unknown -join ', ')" }
    $selected = @($ArenaTable | Where-Object { $_.Name -in $Arenas })
}
$index = 0
foreach ($arena in $selected) {
    $index++
    $dir = Join-Path $ArenasDir $arena.Name
    $label = "[$index/$($selected.Count)] $($arena.Name) ($($arena.Title))"
    if ((Test-ArenaComplete $dir) -and -not $Force) {
        Info "$label present"
        Invoke-Tool $uv @("run", "--no-project", "--python", $Python, (Join-Path $ToolDir "arena_title.py"), $dir, $arena.Title)
        continue
    }
    Info "$label building"
    $started = Get-Date
    Invoke-Tool $uv @(
        "run", "--python", $Python, (Join-Path $ToolDir "build_arena.py"),
        "--name", $arena.Name, "--title", $arena.Title,
        "--center", "$($arena.Center[0])", "$($arena.Center[1])",
        "--size", "$($arena.Size)", "--ymin", "$($arena.YMin)",
        "--dump", $DumpDir, "--out", $ArenasDir, "--cache", $CacheDir
    )
    if (-not $SkipVerify) {
        Invoke-Tool $uv @("run", "--python", $Python, (Join-Path $ToolDir "verify_arena.py"), $dir, "street")
    }
    Info ("$label done in {0:N0}s" -f ((Get-Date) - $started).TotalSeconds)
}

Step "5/5 character animations"
$animBank = Join-Path $DumpDir "bundles\res\animations\antanimations\levels\sp\sp_maincity\sp_maincity_win32_antstate.AssetBank"
if ((Test-Path (Join-Path $AnimsDir "index.json")) -and -not $Force) {
    Info "present ($AnimsDir)"
} elseif (-not $DryRun -and -not (Test-Path $animBank)) {
    Info "skipped: $animBank not in the dump (third person keeps the stock MW2 clips)"
} else {
    Info "decoding Faith's clips into $AnimsDir (~2 min)"
    Invoke-Tool $uv @("run", "--python", $Python, (Join-Path $ToolDir "antanim.py"), "export", $animBank, $AnimsDir)
}

Step "done"
Info "arenas: $ArenasDir"
Info "play:   .\Mirrorwarfare.cmd   (or iw4l.exe map mec:<arena>)"
if ($ArenasDir -ne (Join-Path (Split-Path $RepoDir -Parent) "mec-arenas")) {
    Info "the game finds arenas through IW4L_MEC_ARENAS=$ArenasDir"
}
