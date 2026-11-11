<#
Repeat real Gaia headless 2D hydraulic builds on Windows and compare all
six output channels by SHA-256. Reports process peak WorkingSet, elapsed
time, raw conservation residuals, and PNG determinism. No GPU VRAM measure.
#>
[CmdletBinding()]
param(
  [ValidateRange(32,8192)][int]$Resolution=256,
  [ValidateRange(2,64)][int]$Tiles=2,
  [ValidateRange(1,10)][int]$Repeats=2,
  [string]$Project="",
  [int]$HydraulicNode=2,
  [string]$OutputDirectory="builds/benchmarks-hydraulic",
  [switch]$ClosedEdge,
  [switch]$SkipBuild
)
$ErrorActionPreference="Stop"
$root=(Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$exe=Join-Path $root "target/release/gaia-build.exe"
if (-not $SkipBuild) {
  Push-Location $root
  try {
    & cargo build --release --bin gaia-build
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed ($LASTEXITCODE)" }
  } finally { Pop-Location }
}
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
  throw "Release gaia-build.exe missing. Omit -SkipBuild to compile it."
}
if (-not $Project) {
  $Project=Join-Path $root "tests/fixtures/hydraulic-benchmark.gaia.json"
}
if (-not (Test-Path -LiteralPath $Project -PathType Leaf)) {
  throw "Hydraulic benchmark project not found: $Project"
}
$Project=(Resolve-Path -LiteralPath $Project).Path
if ($Tiles -lt [Math]::Max(2,[Math]::Ceiling($Resolution/256.0))) {
  throw "Too few hydraulic cores: each owned tile must be at most 256 pixels per axis"
}
if (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
  $OutputDirectory=Join-Path $root $OutputDirectory
}
$sessionName="hydro_r"+$Resolution+"_t"+$Tiles+"_"+(Get-Date -Format "yyyyMMdd-HHmmss")+"_"+[guid]::NewGuid().ToString("N").Substring(0,8)
$session=Join-Path $OutputDirectory $sessionName
New-Item -ItemType Directory -Path $session -Force | Out-Null
function Quote-Argument([string]$v) {
  if ($v -match '[\s"]') { return '"' + ($v -replace '"', '\"') + '"' }
  return $v
}
$baseline=$null
$results=@()
for ($run=1;$run -le $Repeats;$run++) {
  $stem=Join-Path $session ("run"+$run)
  $arguments=@(
    "--project",$Project,"--output",$stem,
    "--hydraulic-region","$HydraulicNode",
    "--resolution","$Resolution","--region-tiles","$Tiles","--no-obj"
  )
  if ($ClosedEdge) { $arguments+=@("--hydraulic-edge","closed") }
  $line=($arguments | ForEach-Object { Quote-Argument $_ }) -join " "
  $stdout=Join-Path $session ("run"+$run+".stdout.txt")
  $stderr=Join-Path $session ("run"+$run+".stderr.txt")
  $watch=[Diagnostics.Stopwatch]::StartNew()
  $proc=Start-Process -FilePath $exe -ArgumentList $line -WorkingDirectory $root -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr
  [int64]$peak=0
  while (-not $proc.HasExited) {
    $proc.Refresh()
    $peak=[Math]::Max($peak,[int64]$proc.PeakWorkingSet64)
    Start-Sleep -Milliseconds 50
  }
  $proc.WaitForExit()
  $proc.Refresh()
  $peak=[Math]::Max($peak,[int64]$proc.PeakWorkingSet64)
  $watch.Stop()
  if ($proc.ExitCode -ne 0) {
    throw "Hydraulic run $run failed ($($proc.ExitCode)): $(Get-Content -LiteralPath $stderr -Raw)"
  }
  $folder=$stem+".hydraulic.tiles"
  $reportPath=Join-Path $folder "report.json"
  if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
    throw "Missing hydraulic build report: $reportPath"
  }
  $report=Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
  if ($report.resolution -ne $Resolution -or $report.tile_count -ne $Tiles*$Tiles) {
    throw "Hydraulic report dimensions/tile count do not match request"
  }
  $pngs=@(Get-ChildItem -LiteralPath $folder -Recurse -Filter "*.png" -File |
    Sort-Object { [IO.Path]::GetRelativePath($folder,$_.FullName) })
  if ($pngs.Count -ne 6*$Tiles*$Tiles) {
    throw "Expected $($Tiles*$Tiles*6) PNGs in six channels; found $($pngs.Count)"
  }
  $hashes=@($pngs | ForEach-Object {
    $relative=[IO.Path]::GetRelativePath($folder,$_.FullName)
    "$relative:$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash)"
  })
  $identical=$true
  if ($null -eq $baseline) { $baseline=$hashes }
  else {
    $identical=(($hashes -join "|") -ceq ($baseline -join "|"))
    if (-not $identical) { throw "Hydraulic PNG SHA-256 mismatch on repeated run $run" }
  }
  $row=[pscustomobject]@{
    run=$run
    resolution=$Resolution
    tiles_per_axis=$Tiles
    png_count=$pngs.Count
    wall_ms=$watch.ElapsedMilliseconds
    peak_process_working_set_bytes=$peak
    peak_process_working_set_mib=[Math]::Round($peak/1MB,2)
    water_residual=$report.water_ledger.residual
    solid_residual=$report.solid_ledger.residual
    water_tolerance=$report.water_ledger.tolerance
    solid_tolerance=$report.solid_ledger.tolerance
    identical_pngs_to_first=$identical
  }
  if ([Math]::Abs([double]$row.water_residual) -gt [double]$row.water_tolerance -or
      [Math]::Abs([double]$row.solid_residual) -gt [double]$row.solid_tolerance) {
    throw "Hydraulic conservation budget exceeded on run $run"
  }
  $results+=$row
  Write-Host ("Hydraulic run {0}: {1}ms; peak RSS {2} MiB; PNGs deterministic={3}" -f $run,$row.wall_ms,$row.peak_process_working_set_mib,$identical)
}
$summary=Join-Path $session "benchmark.json"
[pscustomobject]@{
  mode="hydraulic_region"
  project=$Project
  hydraulic_node=$HydraulicNode
  boundary=$(if ($ClosedEdge) { "closed" } else { "open" })
  resolution=$Resolution
  tiles_per_axis=$Tiles
  repeats=$Repeats
  output_folder=$session
  note="Working-set bytes are CPU process memory, not GPU VRAM; benchmark host differs from user machine."
  runs=$results
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $summary -Encoding UTF8
Write-Host "Hydraulic benchmark report: $summary"
