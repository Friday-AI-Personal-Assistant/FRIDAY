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
$workerRoot = Join-Path $env:RUNNER_TEMP 'FridayWorkerSmoke'
$writeRequest = @{ operation = 'execute'; name = 'write_document'; arguments = @{ path = 'smoke.txt'; text = 'Packaged worker file check' }; root = $workerRoot } | ConvertTo-Json -Depth 5 -Compress
$written = $writeRequest | & $worker | ConvertFrom-Json
if (-not $written.ok) { throw "Packaged worker write failed: $($written.error)" }
$readRequest = @{ operation = 'execute'; name = 'read_document'; arguments = @{ path = 'smoke.txt' }; root = $workerRoot } | ConvertTo-Json -Depth 5 -Compress
$read = $readRequest | & $worker | ConvertFrom-Json
if (-not $read.ok -or $read.result -ne 'Packaged worker file check') { throw 'Packaged worker read did not match' }
# Enable local WebView2 debugging for this disposable CI test process only.
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9223 --remote-debugging-address=127.0.0.1 --remote-allow-origins=http://localhost:9223,http://127.0.0.1:9223'
$process = Start-Process -FilePath $app -PassThru
Start-Sleep -Seconds 10
$process.Refresh()
if ($process.HasExited) { throw "Installed application exited early: $($process.ExitCode)" }
node tests/native-smoke.cjs
$nativeResult = $LASTEXITCODE
Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue
if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
if ($nativeResult -eq 2) {
  Write-Warning 'BLOCKED: Native installed WebView2 IPC integration could not run on this runner. Do not claim it passed.'
} elseif ($nativeResult -ne 0) { throw 'Installed native integration test failed' }
Get-Process msedgewebview2 -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
$uninstaller = Join-Path $installDir 'uninstall.exe'
if (-not (Test-Path $uninstaller)) { throw 'Uninstaller missing' }
$uninstall = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
if ($uninstall.ExitCode -ne 0) { throw "Uninstall failed: $($uninstall.ExitCode)" }
Start-Sleep -Seconds 2
if (Test-Path $app) { throw 'Application remained after uninstall' }
Write-Output 'PASS: NSIS install, bundled dependency diagnostics and actual file round trip, application process launch, uninstall.'
Write-Output 'This does not verify interactive UI, microphone, Ollama or desktop automation behavior.'
# A recorded blocked optional debugger check must not leak node's exit code into
# the separately successful install/worker/launch/uninstall smoke result.
$global:LASTEXITCODE = 0
