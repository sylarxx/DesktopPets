[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$ExecutablePath,
  [Parameter(Mandatory = $true)][string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Windows is required.' }
$runtimeExecutable = (Resolve-Path -LiteralPath $ExecutablePath).Path
$runtimeOutput = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $runtimeOutput -Force | Out-Null
if (-not ('HualiVisualSmokeNative' -as [type])) {
  & "$PSScriptRoot/Test-HualiAIWindowsVisualSmoke.ps1" -CompileBackdropOnly
}

$labels = @('mascot', 'panel', 'mascot-menu', 'mascot-notification')
$script:receiptSequence = 0
$script:runtimeProcess = $null
$script:receiptPath = ''
$report = [ordered]@{
  executableSha256 = (Get-FileHash -LiteralPath $runtimeExecutable -Algorithm SHA256).Hash
  sessionTest = 'Simulated native session state; not a physical overnight lock test'
  cases = @()
  passed = $false
}

function Invoke-RuntimeCommand([string]$Command) {
  $trigger = Start-Process -FilePath $runtimeExecutable -ArgumentList "--huali-runtime-smoke=$Command" -PassThru
  if (-not $trigger.WaitForExit(10000)) {
    Stop-Process -Id $trigger.Id -Force -ErrorAction SilentlyContinue
    throw "Single-instance command timed out: $Command"
  }
}

function Read-RuntimeSnapshot {
  try {
    return Get-Content -LiteralPath $script:receiptPath -Raw | ConvertFrom-Json
  } catch { return $null }
}

function Wait-RuntimeSnapshot([scriptblock]$Condition, [int]$TimeoutSeconds = 110) {
  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  do {
    $script:runtimeProcess.Refresh()
    if ($script:runtimeProcess.HasExited) { throw 'The main process exited during recovery.' }
    $script:receiptSequence++
    Invoke-RuntimeCommand "snapshot-$script:receiptSequence"
    Start-Sleep -Milliseconds 1000
    $snapshot = Read-RuntimeSnapshot
    if ($null -ne $snapshot -and (& $Condition $snapshot $script:receiptSequence)) { return $snapshot }
    Start-Sleep -Milliseconds 1000
  } while ([DateTime]::UtcNow -lt $deadline)
  throw "Runtime recovery snapshot timed out: $(ConvertTo-Json $snapshot -Depth 8 -Compress)"
}

function Test-ReadySnapshot($Snapshot, $Sequence) {
  foreach ($label in $labels) {
    $property = $Snapshot.PSObject.Properties[$label]
    if ($null -eq $property) { return $false }
    $view = $property.Value
    if ($view.sequence -ne $Sequence -or -not $view.domPresent -or -not $view.mounted) { return $false }
  }
  return $Snapshot.mascot.nativeVisible -and $Snapshot.mascot.interactive -and $Snapshot.mascot.draftPresent -and
    $Snapshot.'mascot-notification'.nativeVisible -and -not $Snapshot.'mascot-menu'.nativeVisible -and -not $Snapshot.panel.nativeVisible
}

function Get-OwnedWebViewProcesses([int]$RootProcessId) {
  $all = @(Get-CimInstance Win32_Process)
  $owned = [Collections.Generic.HashSet[uint32]]::new()
  $null = $owned.Add([uint32]$RootProcessId)
  do {
    $changed = $false
    foreach ($entry in $all) {
      if ($owned.Contains([uint32]$entry.ParentProcessId) -and $owned.Add([uint32]$entry.ProcessId)) { $changed = $true }
    }
  } while ($changed)
  return @($all | Where-Object { $owned.Contains([uint32]$_.ProcessId) -and $_.Name -eq 'msedgewebview2.exe' })
}

function Save-RuntimeScreen([string]$Name) {
  $bounds = [Windows.Forms.SystemInformation]::VirtualScreen
  $bitmap = [Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
  $graphics = [Drawing.Graphics]::FromImage($bitmap)
  try {
    $graphics.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bounds.Size)
    $bitmap.Save((Join-Path $runtimeOutput "$Name.png"), [Drawing.Imaging.ImageFormat]::Png)
  } finally { $graphics.Dispose(); $bitmap.Dispose() }
}

$previousEnvironment = @{}
foreach ($key in @('HUALI_AI_RELEASE_SMOKE', 'HUALI_AI_RELEASE_SMOKE_NONCE', 'HUALI_AI_RELEASE_SMOKE_AUTH_STATE', 'HUALI_AI_VISUAL_SMOKE_FORCE_MOTION')) {
  $previousEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}
try {
  foreach ($caseName in @('session-lock-unlock', 'renderer-hang', 'renderer-exit', 'browser-exit', 'browser-exit-create-retry')) {
    $nonce = [Guid]::NewGuid().ToString('N')
    $script:receiptPath = Join-Path ([IO.Path]::GetTempPath()) "huali-runtime-smoke-$nonce.json"
    [Environment]::SetEnvironmentVariable('HUALI_AI_RELEASE_SMOKE', '1', 'Process')
    [Environment]::SetEnvironmentVariable('HUALI_AI_RELEASE_SMOKE_NONCE', $nonce, 'Process')
    [Environment]::SetEnvironmentVariable('HUALI_AI_RELEASE_SMOKE_AUTH_STATE', $nonce, 'Process')
    [Environment]::SetEnvironmentVariable('HUALI_AI_VISUAL_SMOKE_FORCE_MOTION', '1', 'Process')
    $case = [ordered]@{ name = $caseName; passed = $false; killedOwnedProcessIds = @() }
    $report.cases += $case
    $script:runtimeProcess = Start-Process -FilePath $runtimeExecutable -PassThru
    $dataDirectory = Join-Path ([IO.Path]::GetTempPath()) "huali-ai-visual-smoke-$($script:runtimeProcess.Id)"
    try {
      Start-Sleep -Seconds 5
      Invoke-RuntimeCommand 'seed'
      $before = Wait-RuntimeSnapshot ${function:Test-ReadySnapshot} 60
      # Startup intentionally shows the mascot. Establish an external foreground
      # before injecting failure, so recovery is tested against a real competing
      # application rather than whatever focus startup happened to retain.
      $screen = [Windows.Forms.SystemInformation]::VirtualScreen
      [HualiVisualSmokeNative]::SetCursorPos($screen.Left + 8, $screen.Top + 8) | Out-Null
      if ([HualiVisualSmokeNative]::SendMouseClick($false) -ne 2) { throw 'Windows rejected the external focus click.' }
      $before = Wait-RuntimeSnapshot {
        param($s, $seq)
        (Test-ReadySnapshot $s $seq) -and -not $s.mascot.foregroundIsApp
      } 15
      $case.before = $before
      if (-not (Test-Path -LiteralPath $dataDirectory -PathType Container)) { throw 'Isolated WebView profile is missing.' }
      $started = [DateTime]::UtcNow
      switch ($caseName) {
        'session-lock-unlock' {
          Invoke-RuntimeCommand 'lock'
          # Longer than the normal renderer timeout. No recovery may occur while locked.
          Start-Sleep -Seconds 40
          $locked = Wait-RuntimeSnapshot { param($s, $seq) $s.mascot.sequence -eq $seq -and -not $s.mascot.interactive } 15
          foreach ($label in $labels) {
            if ($locked.$label.repairs -ne $before.$label.repairs) { throw "Renderer repaired while locked: $label" }
          }
          $case.locked = $locked
          Invoke-RuntimeCommand 'unlock'
        }
        'renderer-hang' { Invoke-RuntimeCommand 'hang-mascot' }
        default {
          if ($caseName -eq 'browser-exit-create-retry') { Invoke-RuntimeCommand 'fail-next-mascot-create' }
          $owned = @(Get-OwnedWebViewProcesses $script:runtimeProcess.Id)
          $targets = if ($caseName -eq 'renderer-exit') {
            @($owned | Where-Object { $_.CommandLine -match '--type=renderer\b' })
          } else {
            @($owned | Where-Object { $_.CommandLine -notmatch '--type=' })
          }
          if ($targets.Count -eq 0) { throw "No owned WebView process found for $caseName." }
          $case.killedOwnedProcessIds = @($targets | ForEach-Object { $_.ProcessId })
          foreach ($target in $targets) { Stop-Process -Id $target.ProcessId -Force -ErrorAction SilentlyContinue }
        }
      }
      $after = Wait-RuntimeSnapshot {
        param($s, $seq)
        (Test-ReadySnapshot $s $seq) -and
        ($s.'mascot-notification'.nativeVisible -eq $before.'mascot-notification'.nativeVisible) -and (
          ($caseName -eq 'session-lock-unlock' -and $s.mascot.epoch -gt $before.mascot.epoch) -or
          ($caseName -ne 'session-lock-unlock' -and $s.mascot.recovered)
        )
      }
      $case.recoverySeconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 2)
      $case.afterRecovery = $after
      if ($after.mascot.foregroundIsApp) { throw 'Recovery activated the desktop assistant instead of preserving the foreground application.' }
      if ($after.panel.nativeVisible -ne $before.panel.nativeVisible) { throw 'Recovery changed the hidden input panel visibility.' }
      if ($after.mascot.processId -ne $script:runtimeProcess.Id) { throw 'Recovery restarted the main process.' }
      if ($caseName -like 'browser-exit*' -and $after.mascot.generation -le $before.mascot.generation) { throw 'Browser exit did not rebuild the native WebView.' }
      if ($caseName -eq 'browser-exit-create-retry' -and $after.mascot.repairs -ne 2) { throw 'Transient creation failure did not consume exactly one retry.' }
      # Deliver a real mouse event at the recovered mascot, then require a fresh JS receipt.
      $pointX = [int]($after.mascot.position.x + $after.mascot.size.width / 2)
      $pointY = [int]($after.mascot.position.y + $after.mascot.size.height / 2)
      [HualiVisualSmokeNative]::SetCursorPos($pointX, $pointY) | Out-Null
      Start-Sleep -Milliseconds 300
      if ([HualiVisualSmokeNative]::SendMouseClick($true) -ne 2) { throw 'Windows rejected mouse input.' }
      $clicked = Wait-RuntimeSnapshot {
        param($s, $seq)
        $s.mascot.sequence -eq $seq -and $s.mascot.pointerCount -gt $after.mascot.pointerCount
      } 15
      $case.after = $clicked
      Save-RuntimeScreen $caseName
      $case.passed = $true
      Write-Host "$caseName passed in $($case.recoverySeconds) seconds. Main PID $($script:runtimeProcess.Id)."
    } finally {
      if ($null -ne $script:runtimeProcess) {
        & taskkill.exe /PID $script:runtimeProcess.Id /T /F 2>&1 | Out-Null
        $script:runtimeProcess = $null
      }
      if (Test-Path -LiteralPath $script:receiptPath) {
        Copy-Item -LiteralPath $script:receiptPath -Destination (Join-Path $runtimeOutput "$caseName-receipt.json") -Force
      }
      Start-Sleep -Seconds 2
      if (Test-Path -LiteralPath $dataDirectory) {
        Remove-Item -LiteralPath $dataDirectory -Recurse -Force
      }
      Remove-Item -LiteralPath $script:receiptPath -Force -ErrorAction SilentlyContinue
    }
  }
  $report.passed = $true
} catch {
  $report.error = $_.Exception.Message
  Save-RuntimeScreen 'failure'
  throw
} finally {
  foreach ($entry in $previousEnvironment.GetEnumerator()) {
    [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
  }
  $report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $runtimeOutput 'runtime-recovery.json') -Encoding UTF8
  $logRoot = Join-Path $env:LOCALAPPDATA 'com.huali.ai.mascot'
  if (Test-Path -LiteralPath $logRoot) {
    Get-ChildItem -LiteralPath $logRoot -Recurse -File -Filter 'runtime-health.jsonl*' |
      Copy-Item -Destination $runtimeOutput -Force
  }
}
