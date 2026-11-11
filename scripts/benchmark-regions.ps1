<#
Run repeatable Gaia regional CLI builds on Windows. Reports wall-clock time,
PeakWorkingSet64, internal peak f32 bytes and per-PNG SHA-256 determinism.
No GPU VRAM measurements: gaia-build is CPU-only.
#>
[CmdletBinding()]
param(
  [ValidateRange(32,8192)][int]$Resolution=4096,
  [ValidateRange(2,64)][int]$Tiles=8,
  [ValidateRange(1,10)][int]$Repeats=2,
  [string]$Project="",
  [string]$Port="1:height",
  [string]$OutputDirectory="builds/benchmarks",
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
if ($Project) {
  if (-not (Test-Path -LiteralPath $Project -PathType Leaf)) {
    throw "Project not found: $Project"
  }
  $Project=(Resolve-Path -LiteralPath $Project).Path
}
if (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
  $OutputDirectory=Join-Path $root $OutputDirectory
}
$sessionName="r"+$Resolution+"_t"+$Tiles+"_"+(Get-Date -Format "yyyyMMdd-HHmmss")+"_"+[guid]::NewGuid().ToString("N").Substring(0,8)
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
  if ($Project) { $arguments=@("--project",$Project) }
  else { $arguments=@("--demo") }
  $arguments+=@("--port",$Port,"--output",$stem,"--resolution","$Resolution","--region-tiles","$Tiles","--no-obj")
  $argumentLine=($arguments | ForEach-Object { Quote-Argument $_ }) -join " "
  $stdout=Join-Path $session ("run"+$run+".stdout.txt")
  $stderr=Join-Path $session ("run"+$run+".stderr.txt")
  $watch=[Diagnostics.Stopwatch]::StartNew()
  $proc=Start-Process -FilePath $exe -ArgumentList $argumentLine -WorkingDirectory $root -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr
  [int64]$peakWorkingSet=0
  while (-not $proc.HasExited) {
    $proc.Refresh()
    $peakWorkingSet=[Math]::Max($peakWorkingSet,[int64]$proc.PeakWorkingSet64)
    Start-Sleep -Milliseconds 50
  }
  $proc.WaitForExit()
  $proc.Refresh()
  $peakWorkingSet=[Math]::Max($peakWorkingSet,[int64]$proc.PeakWorkingSet64)
  $watch.Stop()
  if ($proc.ExitCode -ne 0) {
    throw "Regional build $run failed ($($proc.ExitCode)): $(Get-Content -LiteralPath $stderr -Raw)"
  }
  $reportFile=$stem+".region.report.json"
  if (-not (Test-Path -LiteralPath $reportFile -PathType Leaf)) { throw "Missing $reportFile" }
  $report=Get-Content -LiteralPath $reportFile -Raw | ConvertFrom-Json
  $folder=$stem+".region.tiles"
  $pngs=@(Get-ChildItem -LiteralPath $folder -Filter "*.png" -File | Sort-Object Name)
  if ($pngs.Count -ne $Tiles*$Tiles) { throw "Expected $($Tiles*$Tiles) PNG tiles, found $($pngs.Count)" }
  $hashes=@($pngs | ForEach-Object {
    "$($_.Name):$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash)"
  })
  $identical=$true
  if ($null -eq $baseline) { $baseline=$hashes }
  else {
    $identical=(($hashes -join "|") -ceq ($baseline -join "|"))
    if (-not $identical) { throw "PNG tile SHA-256 mismatch on repeated run $run" }
  }
  $row=[pscustomobject]@{
    run=$run
    resolution=$Resolution
    tiles_per_axis=$Tiles
    tile_count=$pngs.Count
    wall_ms=$watch.ElapsedMilliseconds
    peak_process_working_set_bytes=$peakWorkingSet
    peak_process_working_set_mib=[Math]::Round($peakWorkingSet/1MB,2)
    peak_intermediate_float_bytes=$report.peak_intermediate_bytes
    computed_node_regions=$report.computed_node_regions
    identical_pngs_to_first=$identical
  }
  $results+= $row
  Write-Host ("Run {0}: {1}ms; peak working set {2} MiB; PNG deterministic={3}" -f $run,$row.wall_ms,$row.peak_process_working_set_mib,$identical)
}
$summary=Join-Path $session "benchmark.json"
[pscustomobject]@{
  mode="headless_region"
  project=$(if ($Project) { $Project } else { "demo" })
  port=$Port
  resolution=$Resolution
  tiles_per_axis=$Tiles
  repeats=$Repeats
  output_folder=$session
  note="Working-set bytes represent CPU process memory, not GPU VRAM; wall time is machine dependent."
  runs=$results
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $summary -Encoding UTF8
Write-Host "Benchmark report: $summary"
