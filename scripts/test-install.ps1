$ErrorActionPreference = 'Stop'
$installer = Get-ChildItem 'src-tauri/target/release/bundle/nsis/*.exe' | Select-Object -First 1
if (-not $installer) { throw 'NSIS installer missing' }
$installDir = Join-Path $env:RUNNER_TEMP 'FridayInstallSmoke'
$install = Start-Process -FilePath $installer.FullName -ArgumentList @('/S', "/D=$installDir") -Wait -PassThru
if ($install.ExitCode -ne 0) { throw "Install failed: $($install.ExitCode)" }
$app = Join-Path $installDir 'friday.exe'
$worker = Join-Path $installDir 'worker/friday-worker.exe'
if (-not (Test-Path $app)) { throw 'Installed application missing' }
if (-not (Test-Path $worker)) { throw 'Installed local worker missing' }
$diagnostic = '{"operation":"diagnostics"}' | & $worker | ConvertFrom-Json
if (-not $diagnostic.ok) { throw "Packaged worker failed: $($diagnostic.error)" }
foreach ($dependency in $diagnostic.result.dependencies.PSObject.Properties) {
  if (-not $dependency.Value) { throw "Missing packaged dependency: $($dependency.Name)" }
}
$process = Start-Process -FilePath $app -PassThru
Start-Sleep -Seconds 10
$process.Refresh()
if ($process.HasExited) { throw "Installed application exited early: $($process.ExitCode)" }
Stop-Process -Id $process.Id -Force
Get-Process msedgewebview2 -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
$uninstaller = Join-Path $installDir 'uninstall.exe'
if (-not (Test-Path $uninstaller)) { throw 'Uninstaller missing' }
$uninstall = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
if ($uninstall.ExitCode -ne 0) { throw "Uninstall failed: $($uninstall.ExitCode)" }
Start-Sleep -Seconds 2
if (Test-Path $app) { throw 'Application remained after uninstall' }
Write-Output 'PASS: NSIS install, bundled dependency diagnostics, application process launch, uninstall.'
Write-Output 'This does not verify interactive UI, microphone, Ollama or desktop automation behavior.'
