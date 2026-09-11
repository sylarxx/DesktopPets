[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$MsiPath,

  [Parameter(Mandatory = $true)]
  [string]$ExpectedVersion,

  [string]$PreviousMsiPath = '',

  [string]$PreviousVersion = '1.0.38',

  [string]$PreviousMsiSha256 = '49f38a75d098946deea7c570df7911a46bc3f361a935c58279a073cc1d00655a',

  [switch]$RunVisualSmoke,

  [string]$VisualSmokeOutputDirectory = '',

  [switch]$ValidateDefaultLaunch,

  [switch]$RequireInteractiveDefaultLaunch,

  [switch]$RequireRawAuthDiagnostics,

  [switch]$RequireDiagnosticLogCleanup,

  [switch]$DiagnosticOnly,

  [string]$EvidenceDirectory = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ProductName = '华力AI桌面助手'
$ProcessName = 'HualiAIDesktopAssistant'
$MarkerPath = 'HKLM:\SOFTWARE\Huali\HualiAIDesktopAssistant'
$RunPath = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run'
$RunValueName = '华力AI桌面助手'
$ProtocolSubKey = 'SOFTWARE\Classes\huali-ai-mascot'
$LaunchLogPath = Join-Path $env:ProgramData 'HualiAI\Logs\launch-after-install.log'

function Test-IsAdministrator {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Invoke-MsiExec {
  param(
    [Parameter(Mandatory = $true)][string[]]$Arguments,
    [int[]]$AllowedExitCodes = @(0, 3010),
    [int]$TimeoutSeconds = 900
  )

  $process = Start-Process -FilePath 'msiexec.exe' -ArgumentList $Arguments -PassThru
  if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    throw "Windows Installer 在 $TimeoutSeconds 秒内未结束，疑似静默部署挂起。"
  }
  $process.Refresh()
  if ($process.ExitCode -notin $AllowedExitCodes) {
    throw "Windows Installer 返回失败码：$($process.ExitCode)"
  }
  return $process.ExitCode
}

function Get-InstalledProducts {
  $roots = @(
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
  )
  return @(Get-ItemProperty -Path $roots -ErrorAction SilentlyContinue |
    Where-Object {
      $displayName = $_.PSObject.Properties['DisplayName']
      $windowsInstaller = $_.PSObject.Properties['WindowsInstaller']
      $displayName -and $windowsInstaller -and
        $displayName.Value -eq $ProductName -and $windowsInstaller.Value -eq 1
    } |
    Sort-Object PSPath -Unique)
}

function Get-RunExecutablePath {
  $runKey = Get-ItemProperty -LiteralPath $RunPath -ErrorAction SilentlyContinue
  $runProperty = if ($runKey) { $runKey.PSObject.Properties[$RunValueName] } else { $null }
  if (-not $runProperty) {
    throw 'HKLM 登录自启动项不存在。'
  }
  $runValue = [string]$runProperty.Value
  if ($runValue -notmatch '^"?(.+?HualiAIDesktopAssistant\.exe)"?$') {
    throw "HKLM 登录自启动项格式不正确：$runValue"
  }
  return $Matches[1]
}

function Assert-DesktopAuthProtocol {
  param([Parameter(Mandatory = $true)][string]$ExpectedExecutablePath)

  $baseKey = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
    [Microsoft.Win32.RegistryHive]::LocalMachine,
    [Microsoft.Win32.RegistryView]::Registry64
  )
  try {
    $protocolKey = $baseKey.OpenSubKey($ProtocolSubKey)
    if (-not $protocolKey) {
      throw '机器级 huali-ai-mascot 登录回调协议未注册。'
    }
    try {
      if ('URL Protocol' -notin @($protocolKey.GetValueNames())) {
        throw 'huali-ai-mascot 注册缺少 URL Protocol 标记。'
      }
    } finally {
      $protocolKey.Dispose()
    }

    $commandKey = $baseKey.OpenSubKey("$ProtocolSubKey\shell\open\command")
    if (-not $commandKey) {
      throw 'huali-ai-mascot 注册缺少 shell open command。'
    }
    try {
      $command = [string]$commandKey.GetValue('')
      $commandMatch = [regex]::Match($command, '^\s*"(?<executable>[^"]+)"\s+"%1"\s*$')
      if (-not $commandMatch.Success) {
        throw "huali-ai-mascot 回调命令错误：$command"
      }
      $registeredExecutablePath = $commandMatch.Groups['executable'].Value
      if (-not (Test-Path -LiteralPath $registeredExecutablePath -PathType Leaf)) {
        throw "huali-ai-mascot 回调程序不存在：$registeredExecutablePath"
      }

      # MSI's [!Path] formatter is allowed to persist an 8.3 short path. A
      # content identity check accepts that valid representation while still
      # proving the protocol launches the installed desktop executable.
      $registeredHash = (Get-FileHash -LiteralPath $registeredExecutablePath -Algorithm SHA256).Hash
      $expectedHash = (Get-FileHash -LiteralPath $ExpectedExecutablePath -Algorithm SHA256).Hash
      if ($registeredHash -ne $expectedHash) {
        throw "huali-ai-mascot 回调程序与已安装主程序不一致：$registeredExecutablePath"
      }
    } finally {
      $commandKey.Dispose()
    }
  } finally {
    $baseKey.Dispose()
  }
}

function Assert-NoDesktopAuthProtocol {
  param([Parameter(Mandatory = $true)][string]$Stage)

  $baseKey = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
    [Microsoft.Win32.RegistryHive]::LocalMachine,
    [Microsoft.Win32.RegistryView]::Registry64
  )
  try {
    $protocolKey = $baseKey.OpenSubKey($ProtocolSubKey)
    if ($protocolKey) {
      $protocolKey.Dispose()
      throw "$Stage 后仍残留 huali-ai-mascot 登录回调协议。"
    }
  } finally {
    $baseKey.Dispose()
  }
}

function New-DesktopDiagnosticCleanupFixture {
  $logDirectory = Join-Path $env:LOCALAPPDATA 'com.huali.ai.mascot\logs'
  New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
  $primaryPath = Join-Path $logDirectory 'desktop-diagnostic.jsonl'
  $rotatedPath = Join-Path $logDirectory 'desktop-diagnostic.jsonl.1'
  $retainedPath = Join-Path $logDirectory 'formal-release-retained-sentinel.txt'
  Set-Content -LiteralPath $primaryPath -Value 'legacy-primary' -Encoding UTF8
  Set-Content -LiteralPath $rotatedPath -Value 'legacy-rotated' -Encoding UTF8
  Set-Content -LiteralPath $retainedPath -Value 'retain' -Encoding UTF8
  return [ordered]@{
    primaryPath = $primaryPath
    rotatedPath = $rotatedPath
    retainedPath = $retainedPath
  }
}

function Assert-DesktopDiagnosticCleanup {
  param([Parameter(Mandatory = $true)]$Fixture)

  if (Test-Path -LiteralPath $Fixture.primaryPath) {
    throw "正式版启动后仍残留诊断日志：$($Fixture.primaryPath)"
  }
  if (Test-Path -LiteralPath $Fixture.rotatedPath) {
    throw "正式版启动后仍残留轮转诊断日志：$($Fixture.rotatedPath)"
  }
  if (-not (Test-Path -LiteralPath $Fixture.retainedPath -PathType Leaf)) {
    throw '诊断日志清理误删了非目标用户文件。'
  }
}

function Invoke-DesktopAuthProtocolCallbackSmoke {
  param(
    [Parameter(Mandatory = $true)][string]$ExpectedExecutablePath,
    [Parameter(Mandatory = $true)][string]$ExpectedAppVersion,
    [Parameter(Mandatory = $true)][string]$DiagnosticEvidenceDirectory,
    [switch]$RequireRawDiagnostics,
    [switch]$DiagnosticOnly
  )

  $nonce = [Guid]::NewGuid().ToString('N')
  $receiptPath = Join-Path $env:TEMP "huali-ai-desktop-auth-smoke-$nonce.json"
  $authState = "smoke-$nonce"
  # Windows protocol activation is case-insensitive, while URL handling for a
  # custom scheme may preserve host casing. Exercise that production variant
  # so renderer validation cannot regress to a case-sensitive comparison.
  $callbackUrl = "HUALI-AI-MASCOT://AUTH-CALLBACK/?state=$authState&token=smoke-token&userId=smoke-user&smokeNonce=$nonce"
  $previousSmokeEnabled = $env:HUALI_AI_RELEASE_SMOKE
  $previousSmokeState = $env:HUALI_AI_RELEASE_SMOKE_AUTH_STATE
  $previousSmokeNonce = $env:HUALI_AI_RELEASE_SMOKE_NONCE

  try {
    Remove-Item -LiteralPath $receiptPath -Force -ErrorAction SilentlyContinue
    $env:HUALI_AI_RELEASE_SMOKE = '1'
    $env:HUALI_AI_RELEASE_SMOKE_AUTH_STATE = $authState
    $env:HUALI_AI_RELEASE_SMOKE_NONCE = $nonce

    # Run the real renderer first. Starting only from the protocol URL proves
    # that a short-lived helper process saw argv, but not that single-instance
    # forwarding reached the already-running app or its Vue auth listener.
    $existingProcess = Start-Process -FilePath $ExpectedExecutablePath -PassThru
    $existingWindowHandle = Wait-ForVisibleApplicationWindow `
      -Process $existingProcess `
      -TimeoutSeconds 20

    # This deliberately invokes the registered URI rather than passing the URL
    # to the executable. The final receipt proves Windows shell activation,
    # existing-instance forwarding and renderer-side parsing as one chain.
    Start-Process -FilePath $callbackUrl | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    $receipt = $null
    while ($null -eq $receipt) {
      if ([DateTime]::UtcNow -ge $deadline) {
        throw 'huali-ai-mascot 真协议回调未在 30 秒内交付到运行中 renderer。'
      }
      if (Test-Path -LiteralPath $receiptPath -PathType Leaf) {
        try {
          $candidate = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
          $callbackCommitted = $candidate.forwardedToRunningInstance -eq $true -and
            $candidate.rendererReceived -eq $true -and
            $candidate.rendererOutcome -eq 'success' -and
            $candidate.sessionCommitted -eq $true
          $formalReleaseSignalsReady = $false
          if (-not $DiagnosticOnly) {
            $formalReleaseSignalsReady = $candidate.subscriptionsStarted -eq $true -and
              $candidate.reminderTypesQueued -eq 3 -and
              $candidate.notificationWindowShown -eq $true
          }
          if ($callbackCommitted -and ($DiagnosticOnly -or $formalReleaseSignalsReady)) {
            $receipt = $candidate
            break
          }
        } catch {
          # The native process may be replacing the tiny JSON receipt between
          # stages. Retry until the complete renderer receipt is available.
        }
      }
      Start-Sleep -Milliseconds 200
    }

    $requiredReceiptFields = @(
        'callbackReceived',
        'hasState',
        'hasToken',
        'hasUserId',
        'forwardedToRunningInstance',
        'rendererReceived',
        'sessionCommitted'
      )
    if (-not $DiagnosticOnly) {
      $requiredReceiptFields += @('subscriptionsStarted', 'notificationWindowShown')
    }
    foreach ($field in $requiredReceiptFields) {
      $property = $receipt.PSObject.Properties[$field]
      if (-not $property -or $property.Value -ne $true) {
        throw "huali-ai-mascot 真协议回调缺少必要字段：$field"
      }
    }
    if ($receipt.rendererOutcome -ne 'success') {
      throw "renderer 未完成有效 state 的登录提交：$($receipt.rendererOutcome)"
    }
    if (-not $DiagnosticOnly -and $receipt.reminderTypesQueued -ne 3) {
      throw "待办、会议、消息三类提醒未全部进入 renderer 队列：$($receipt.reminderTypesQueued)"
    }
    if (-not $DiagnosticOnly -and $receipt.notificationCompact -ne $false) {
      throw '原生提醒窗口仍是登录卡片尺寸，系统消息提醒未真正展示。'
    }

    $rawDiagnosticsVerified = $false
    if ($RequireRawDiagnostics) {
      $diagnosticLogPath = Join-Path $env:LOCALAPPDATA 'com.huali.ai.mascot\logs\desktop-diagnostic.jsonl'
      $diagnosticDeadline = [DateTime]::UtcNow.AddSeconds(20)
      $nativeDiagnostic = $null
      $rendererDiagnostic = $null
      $sessionDiagnostic = $null
      $nativeRawComplete = $false
      $rendererRawComplete = $false
      while ([DateTime]::UtcNow -lt $diagnosticDeadline) {
        if (Test-Path -LiteralPath $diagnosticLogPath -PathType Leaf) {
          $versionFragment = '"appVersion":"' + $ExpectedAppVersion + '"'
          $stateFragment = '"' + $authState + '"'
          $diagnosticLines = @(Get-Content -LiteralPath $diagnosticLogPath -ErrorAction Stop)
          $nativeDiagnosticLine = $diagnosticLines |
            Where-Object {
              $_.Contains($versionFragment) -and
              $_.Contains('"event":"auth.callback.single_instance_received"') -and
              $_.Contains('"token":"smoke-token"') -and
              $_.Contains($stateFragment)
            } |
            Select-Object -Last 1
          $rendererDiagnosticLine = $diagnosticLines |
            Where-Object {
              $_.Contains($versionFragment) -and
              $_.Contains('"event":"auth.callback.renderer_parsed"') -and
              $_.Contains('"outcome":"success"') -and
              $_.Contains('"token":"smoke-token"') -and
              $_.Contains($stateFragment)
            } |
            Select-Object -Last 1
          $sessionDiagnosticLine = $diagnosticLines |
            Where-Object {
              $_.Contains($versionFragment) -and
              $_.Contains('"event":"session.store.committed"') -and
              $_.Contains('"token":"smoke-token"') -and
              $_.Contains('"userId":"smoke-user"')
            } |
            Select-Object -Last 1
          $nativeDiagnostic = if ($nativeDiagnosticLine) {
            $nativeDiagnosticLine | ConvertFrom-Json -AsHashtable
          } else { $null }
          $rendererDiagnostic = if ($rendererDiagnosticLine) {
            $rendererDiagnosticLine | ConvertFrom-Json -AsHashtable
          } else { $null }
          $sessionDiagnostic = if ($sessionDiagnosticLine) {
            $sessionDiagnosticLine | ConvertFrom-Json -AsHashtable
          } else { $null }
          $nativeFields = if ($nativeDiagnostic) { $nativeDiagnostic['fields'] } else { $null }
          $rendererFields = if ($rendererDiagnostic) { $rendererDiagnostic['fields'] } else { $null }
          $sessionFields = if ($sessionDiagnostic) { $sessionDiagnostic['fields'] } else { $null }
          $nativeFieldsMatch = $null -ne $nativeDiagnostic -and
            $nativeFields['callbackPrefixMatches'] -eq $true -and
            $nativeFields['token'] -eq 'smoke-token' -and
            $nativeFields['state'] -eq $authState
          $rendererFieldsMatch = $null -ne $rendererDiagnostic -and
            $rendererFields['receivedState'] -eq $authState -and
            $rendererFields['expectedState'] -eq $authState -and
            $rendererFields['stateMatches'] -eq $true
          $sessionFieldsMatch = $null -ne $sessionDiagnostic -and
            $sessionFields['token'] -eq 'smoke-token' -and
            $sessionFields['userId'] -eq 'smoke-user'
          $nativeRawComplete = $nativeFieldsMatch -and
            [string]$nativeFields['callbackUrl'] -notin @('', '[redacted]') -and
            $nativeFields['callbackUrlLength'] -eq ([string]$nativeFields['callbackUrl']).Length
          $rendererRawComplete = $rendererFieldsMatch -and
            [string]$rendererFields['rawUrl'] -notin @('', '[redacted]') -and
            $rendererFields['rawUrlLength'] -eq ([string]$rendererFields['rawUrl']).Length
          if ($nativeRawComplete -and $rendererRawComplete -and $sessionFieldsMatch) {
            $rawDiagnosticsVerified = $true
            break
          }
        }
        Start-Sleep -Milliseconds 200
      }
      if (Test-Path -LiteralPath $diagnosticLogPath -PathType Leaf) {
        Copy-Item `
          -LiteralPath $diagnosticLogPath `
          -Destination (Join-Path $DiagnosticEvidenceDirectory 'raw-auth-diagnostic.jsonl') `
          -Force
      }
      if (-not $rawDiagnosticsVerified) {
        throw "本地诊断日志字段不完整：nativeEvent=$([bool]$nativeDiagnostic)，nativeRaw=$nativeRawComplete，rendererEvent=$([bool]$rendererDiagnostic)，rendererRaw=$rendererRawComplete，session=$([bool]$sessionDiagnostic)，path=$diagnosticLogPath"
      }
    }
    # The protocol helper can still be shutting down for a fraction of a
    # second after the running renderer has written its receipt. Give Windows
    # a short, bounded grace period before asserting the single-instance gate.
    $singleInstanceDeadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
      $runningProcesses = @(Get-HualiProcesses)
      if ($runningProcesses.Count -eq 1 -and
          $runningProcesses[0].Id -eq $existingProcess.Id) {
        break
      }
      Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $singleInstanceDeadline)

    if ($runningProcesses.Count -ne 1 -or $runningProcesses[0].Id -ne $existingProcess.Id) {
      throw "协议回调没有保持单实例：启动前=$($existingProcess.Id)，当前=$($runningProcesses.Id -join ',')"
    }

    if ($DiagnosticOnly) {
      return [ordered]@{
        shellProtocolInvoked = $true
        nativeCallbackReceived = $true
        forwardedToRunningInstance = $true
        rendererReceived = $true
        rendererStateValidation = $receipt.rendererOutcome
        sessionCommitted = $true
        singleInstancePreserved = $true
        existingWindowHandle = $existingWindowHandle
        stateDelivered = $true
        tokenDelivered = $true
        userIdDelivered = $true
        tokenValueRecorded = $rawDiagnosticsVerified
        completeCallbackUrlRecorded = $rawDiagnosticsVerified
        expectedAndReceivedStateRecorded = $rawDiagnosticsVerified
      }
    }

    & (Join-Path $PSScriptRoot 'Test-HualiAIMessageDrag.ps1') `
      -ApplicationProcessId $existingProcess.Id `
      -OutputDirectory (Join-Path $DiagnosticEvidenceDirectory 'message-drag')

    # A successful callback is not sufficient if the session disappears with
    # the renderer process. Restart the installed executable against the same
    # WebView2 profile and require the persisted session to be restored.
    Stop-Process -Id $existingProcess.Id -Force
    $stoppedDeadline = [DateTime]::UtcNow.AddSeconds(10)
    while (@(Get-HualiProcesses).Count -gt 0 -and [DateTime]::UtcNow -lt $stoppedDeadline) {
      Start-Sleep -Milliseconds 200
    }
    Assert-NoHualiProcesses -Stage '有效登录 callback 后重启'
    $restartProcess = Start-Process -FilePath $ExpectedExecutablePath -PassThru
    $restartWindowHandle = Wait-ForVisibleApplicationWindow `
      -Process $restartProcess `
      -TimeoutSeconds 20
    $restartDeadline = [DateTime]::UtcNow.AddSeconds(20)
    $sessionRestoredAfterRestart = $false
    while (-not $sessionRestoredAfterRestart -and [DateTime]::UtcNow -lt $restartDeadline) {
      if (Test-Path -LiteralPath $receiptPath -PathType Leaf) {
        try {
          $restartReceipt = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
          $sessionRestoredAfterRestart = $restartReceipt.sessionRestoredAfterRestart -eq $true
        } catch {
          $sessionRestoredAfterRestart = $false
        }
      }
      if (-not $sessionRestoredAfterRestart) { Start-Sleep -Milliseconds 200 }
    }
    if (-not $sessionRestoredAfterRestart) {
      throw '有效 callback 写入的桌面 session 在进程重启后没有恢复。'
    }

    return [ordered]@{
      shellProtocolInvoked = $true
      nativeCallbackReceived = $true
      forwardedToRunningInstance = $true
      rendererReceived = $true
      rendererStateValidation = $receipt.rendererOutcome
      sessionCommitted = $true
      subscriptionsStarted = $true
      todoMeetingMessageQueued = $true
      notificationWindowShown = $true
      notificationProcessId = $receipt.notificationProcessId
      singleInstancePreserved = $true
      existingWindowHandle = $existingWindowHandle
      sessionRestoredAfterRestart = $true
      restartProcessId = $restartProcess.Id
      restartWindowHandle = $restartWindowHandle
      stateDelivered = $true
      tokenDelivered = $true
      userIdDelivered = $true
      tokenValueRecorded = $rawDiagnosticsVerified
      completeCallbackUrlRecorded = $rawDiagnosticsVerified
      expectedAndReceivedStateRecorded = $rawDiagnosticsVerified
    }
  } finally {
    Remove-Item -LiteralPath $receiptPath -Force -ErrorAction SilentlyContinue
    if ($null -eq $previousSmokeEnabled) {
      Remove-Item Env:HUALI_AI_RELEASE_SMOKE -ErrorAction SilentlyContinue
    } else {
      $env:HUALI_AI_RELEASE_SMOKE = $previousSmokeEnabled
    }
    if ($null -eq $previousSmokeState) {
      Remove-Item Env:HUALI_AI_RELEASE_SMOKE_AUTH_STATE -ErrorAction SilentlyContinue
    } else {
      $env:HUALI_AI_RELEASE_SMOKE_AUTH_STATE = $previousSmokeState
    }
    if ($null -eq $previousSmokeNonce) {
      Remove-Item Env:HUALI_AI_RELEASE_SMOKE_NONCE -ErrorAction SilentlyContinue
    } else {
      $env:HUALI_AI_RELEASE_SMOKE_NONCE = $previousSmokeNonce
    }
  }
}

function Get-HualiProcesses {
  return @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue)
}

function Stop-HualiProcesses {
  Get-HualiProcesses | Stop-Process -Force -ErrorAction SilentlyContinue
  $deadline = [DateTime]::UtcNow.AddSeconds(10)
  while (@(Get-HualiProcesses).Count -gt 0 -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 100
  }
  if (@(Get-HualiProcesses).Count -gt 0) {
    throw '无法停止华力 AI 桌面助手进程。'
  }
}

function Assert-NoHualiProcesses {
  param([Parameter(Mandatory = $true)][string]$Stage)

  $processes = @(Get-HualiProcesses)
  if ($processes.Count -gt 0) {
    throw "$Stage 后仍有 $($processes.Count) 个华力 AI 桌面助手进程。"
  }
}

function Assert-InstalledState {
  param([Parameter(Mandatory = $true)][string]$Version)

  $products = @(Get-InstalledProducts)
  if ($products.Count -ne 1) {
    throw "安装登记数量错误：实际=$($products.Count)，预期=1。"
  }
  $product = $products[0]
  if ([string]$product.DisplayVersion -ne $Version) {
    throw "安装版本错误：实际=$($product.DisplayVersion)，预期=$Version。"
  }

  $marker = Get-ItemProperty -LiteralPath $MarkerPath -ErrorAction Stop
  if ($marker.InstallerType -ne 'MSI' -or [string]$marker.Version -ne $Version) {
    throw "整机安装检测标记错误：InstallerType=$($marker.InstallerType)，Version=$($marker.Version)。"
  }

  $mainExecutable = Get-RunExecutablePath
  if (-not (Test-Path -LiteralPath $mainExecutable -PathType Leaf)) {
    throw "安装目录缺少主程序：$mainExecutable"
  }
  $separator = [IO.Path]::DirectorySeparatorChar
  $programFilesRoot = [IO.Path]::GetFullPath($env:ProgramFiles).TrimEnd($separator) + $separator
  $installedExecutablePath = [IO.Path]::GetFullPath($mainExecutable)
  if (-not $installedExecutablePath.StartsWith($programFilesRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "程序未按计算机安装到 Program Files：$installedExecutablePath"
  }
  Assert-DesktopAuthProtocol -ExpectedExecutablePath $installedExecutablePath

  return [pscustomobject]@{
    ProductCode = [string]$product.PSChildName
    Version = [string]$product.DisplayVersion
    ExecutablePath = $installedExecutablePath
    ExecutableSha256 = (Get-FileHash -LiteralPath $installedExecutablePath -Algorithm SHA256).Hash.ToLowerInvariant()
  }
}

function Assert-UninstalledState {
  param(
    [Parameter(Mandatory = $true)][string]$Stage,
    [string]$FormerExecutablePath = ''
  )

  $products = @(Get-InstalledProducts)
  if ($products.Count -ne 0) {
    throw "$Stage 后仍存在 $($products.Count) 条 Windows Installer 产品登记。"
  }
  if (Test-Path -LiteralPath $MarkerPath) {
    throw "$Stage 后仍存在整机安装检测标记。"
  }
  $runKey = Get-ItemProperty -LiteralPath $RunPath -ErrorAction SilentlyContinue
  if ($runKey -and $runKey.PSObject.Properties[$RunValueName]) {
    throw "$Stage 后仍存在 HKLM 登录自启动项。"
  }
  Assert-NoDesktopAuthProtocol -Stage $Stage
  if ($FormerExecutablePath -and (Test-Path -LiteralPath $FormerExecutablePath -PathType Leaf)) {
    throw "$Stage 后主程序仍残留：$FormerExecutablePath"
  }
  if ($FormerExecutablePath) {
    $formerInstallDirectory = Split-Path -Parent $FormerExecutablePath
    if (Test-Path -LiteralPath $formerInstallDirectory) {
      throw "$Stage 后安装目录仍残留：$formerInstallDirectory"
    }
  }
  Assert-NoHualiProcesses -Stage $Stage
}

function Invoke-Install {
  param(
    [Parameter(Mandatory = $true)][string]$ResolvedMsi,
    [Parameter(Mandatory = $true)][string]$LogPath,
    [switch]$DisableLaunch
  )

  $arguments = @(
    '/i', ('"{0}"' -f $ResolvedMsi),
    '/qn', '/norestart', 'REBOOT=ReallySuppress',
    '/L*v', ('"{0}"' -f $LogPath)
  )
  if ($DisableLaunch) {
    $arguments += 'HUALI_START_AFTER_INSTALL=0'
  }
  Invoke-MsiExec -Arguments $arguments | Out-Null
}

function Invoke-UninstallProduct {
  param(
    [Parameter(Mandatory = $true)][string]$ProductCode,
    [Parameter(Mandatory = $true)][string]$LogPath
  )

  Invoke-MsiExec -Arguments @(
    '/x', $ProductCode,
    '/qn', '/norestart', 'REBOOT=ReallySuppress',
    '/L*v', ('"{0}"' -f $LogPath)
  ) | Out-Null
}

function Get-InteractiveExplorerSessions {
  return @(Get-Process -Name 'explorer' -ErrorAction SilentlyContinue |
      Where-Object { $_.SessionId -gt 0 } |
      Select-Object -ExpandProperty SessionId -Unique)
}

function Wait-ForDefaultLaunch {
  param(
    [Parameter(Mandatory = $true)][string]$ExecutablePath,
    [Parameter(Mandatory = $true)][int[]]$InteractiveSessions,
    [int]$TimeoutSeconds = 30
  )

  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  do {
    foreach ($process in Get-HualiProcesses) {
      $path = ''
      try {
        $path = [string]$process.Path
      } catch {
        $path = ''
      }
      if ($path -and
          [IO.Path]::GetFullPath($path).Equals(
            [IO.Path]::GetFullPath($ExecutablePath),
            [StringComparison]::OrdinalIgnoreCase
          ) -and
          $process.SessionId -in $InteractiveSessions -and
          [HualiDeploymentSmokeNative]::HasVisibleApplicationWindow($process.Id)) {
        return $process
      }
    }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)

  throw '默认安装完成后，未在已登录交互用户会话找到主程序及可见窗口。'
}

function Get-LaunchLogLineCount {
  if (-not (Test-Path -LiteralPath $LaunchLogPath -PathType Leaf)) {
    return 0
  }
  return @(Get-Content -LiteralPath $LaunchLogPath -ErrorAction Stop).Count
}

function Wait-ForNewLaunchLogLine {
  param(
    [Parameter(Mandatory = $true)][int]$StartingLineCount,
    [Parameter(Mandatory = $true)][string]$Pattern,
    [int]$TimeoutSeconds = 20
  )

  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  do {
    if (Test-Path -LiteralPath $LaunchLogPath -PathType Leaf) {
      $allLines = @(Get-Content -LiteralPath $LaunchLogPath -ErrorAction Stop)
      if ($allLines.Count -gt $StartingLineCount) {
        $newLines = @($allLines | Select-Object -Skip $StartingLineCount)
        $match = $newLines | Where-Object { $_ -match $Pattern } | Select-Object -Last 1
        if ($match) {
          return [string]$match
        }
      }
    }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)

  throw "安装后启动辅助程序未在新日志中写入预期状态：$Pattern"
}

function Wait-ForVisibleApplicationWindow {
  param(
    [Parameter(Mandatory = $true)][Diagnostics.Process]$Process,
    [int]$TimeoutSeconds = 30
  )

  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  do {
    $Process.Refresh()
    if ($Process.HasExited) {
      throw "覆盖升级前启动的旧版程序提前退出，退出码=$($Process.ExitCode)。"
    }
    $windowHandle = [HualiDeploymentSmokeNative]::FindVisibleApplicationWindow($Process.Id)
    if ($windowHandle -ne [IntPtr]::Zero) {
      return $windowHandle.ToInt64()
    }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)

  throw "旧版程序已启动，但 $TimeoutSeconds 秒内没有出现可见主窗口。"
}

if ($env:OS -ne 'Windows_NT') {
  throw '该脚本只能在 Windows 上运行。'
}
if (-not (Test-IsAdministrator)) {
  throw '静默部署冒烟测试必须由管理员运行。'
}
if ($RequireInteractiveDefaultLaunch -and -not $ValidateDefaultLaunch) {
  throw '-RequireInteractiveDefaultLaunch 必须与 -ValidateDefaultLaunch 同时使用。'
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class HualiDeploymentSmokeNative
{
    private static readonly IntPtr PerMonitorAwareV2 = new IntPtr(-4);

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    private delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetProcessDpiAwarenessContext(IntPtr value);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);

    [DllImport("user32.dll")]
    private static extern IntPtr GetThreadDpiAwarenessContext();

    [DllImport("user32.dll")]
    private static extern bool AreDpiAwarenessContextsEqual(IntPtr first, IntPtr second);

    public static bool EnablePerMonitorV2()
    {
        SetProcessDpiAwarenessContext(PerMonitorAwareV2);
        if (SetThreadDpiAwarenessContext(PerMonitorAwareV2) == IntPtr.Zero)
        {
            return false;
        }
        return AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            PerMonitorAwareV2
        );
    }

    public static IntPtr FindVisibleApplicationWindow(int processId)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hWnd, _) =>
        {
            uint owner;
            RECT rect;
            GetWindowThreadProcessId(hWnd, out owner);
            if (owner == processId && IsWindowVisible(hWnd) && GetWindowRect(hWnd, out rect) &&
                rect.Right - rect.Left >= 100 && rect.Bottom - rect.Top >= 80)
            {
                found = hWnd;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static bool HasVisibleApplicationWindow(int processId)
    {
        return FindVisibleApplicationWindow(processId) != IntPtr.Zero;
    }
}
'@

if (-not [HualiDeploymentSmokeNative]::EnablePerMonitorV2()) {
  throw '无法将 Windows 部署验收线程设置为 Per-Monitor-V2 DPI 模式。'
}

if (-not $EvidenceDirectory.Trim()) {
  $EvidenceDirectory = Join-Path $env:TEMP 'HualiAI-deployment-smoke'
}
$resolvedEvidence = [IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $resolvedEvidence -Force | Out-Null
if (-not $VisualSmokeOutputDirectory.Trim()) {
  $VisualSmokeOutputDirectory = Join-Path $resolvedEvidence 'windows-visual-smoke'
}
$resolvedVisualEvidence = [IO.Path]::GetFullPath($VisualSmokeOutputDirectory)

$resolvedMsi = ''
$resolvedPreviousMsi = ''

$report = [ordered]@{
  expectedVersion = $ExpectedVersion
  currentMsiSha256 = $null
  previousVersion = if ($PreviousMsiPath.Trim()) { $PreviousVersion } else { $null }
  previousMsiExpectedSha256 = if ($PreviousMsiPath.Trim()) { $PreviousMsiSha256.ToLowerInvariant() } else { $null }
  previousMsiActualSha256 = $null
  dpiAwareness = 'PerMonitorAwareV2'
  startedAt = [DateTime]::UtcNow.ToString('o')
  ok = $false
  failure = $null
  cleanup = $null
  checks = [ordered]@{}
}
$primaryFailure = $null
$cleanupFailure = $null
$cleanupAuthorized = $false
$lastExecutablePath = ''
$diagnosticCleanupFixture = $null

try {
  if ($ExpectedVersion -notmatch '^\d+\.\d+\.\d+$') {
    throw "预期版本号必须是三段数字：$ExpectedVersion"
  }
  $resolvedMsi = (Resolve-Path -LiteralPath $MsiPath).Path
  if ([IO.Path]::GetExtension($resolvedMsi) -ne '.msi') {
    throw "当前安装包不是 MSI：$resolvedMsi"
  }
  $report.currentMsiSha256 = (Get-FileHash -LiteralPath $resolvedMsi -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($PreviousMsiPath.Trim()) {
    if ($PreviousMsiSha256 -notmatch '^[0-9a-fA-F]{64}$') {
      throw "上一版 MSI SHA256 格式不正确：$PreviousMsiSha256"
    }
    $resolvedPreviousMsi = (Resolve-Path -LiteralPath $PreviousMsiPath).Path
    if ([IO.Path]::GetExtension($resolvedPreviousMsi) -ne '.msi') {
      throw "上一版安装包不是 MSI：$resolvedPreviousMsi"
    }
    $actualPreviousHash = (Get-FileHash -LiteralPath $resolvedPreviousMsi -Algorithm SHA256).Hash.ToLowerInvariant()
    $report.previousMsiActualSha256 = $actualPreviousHash
    if ($actualPreviousHash -ne $PreviousMsiSha256.Trim().ToLowerInvariant()) {
      throw "上一版 MSI SHA256 错误：实际=$actualPreviousHash，预期=$PreviousMsiSha256。"
    }
  }

  Assert-UninstalledState -Stage '测试开始前'
  $cleanupAuthorized = $true

  if ($resolvedPreviousMsi) {
    Write-Host "验证 $PreviousVersion 管理员 /qn 基线安装..."
    Invoke-Install `
      -ResolvedMsi $resolvedPreviousMsi `
      -LogPath (Join-Path $resolvedEvidence '01-previous-install.log') `
      -DisableLaunch
    $previousState = Assert-InstalledState -Version $PreviousVersion
    $lastExecutablePath = $previousState.ExecutablePath
    Assert-NoHualiProcesses -Stage "$PreviousVersion 禁止启动安装"
    $report.checks.previousInstall = $previousState

    $previousProcess = Start-Process `
      -FilePath $previousState.ExecutablePath `
      -WorkingDirectory (Split-Path -Parent $previousState.ExecutablePath) `
      -PassThru
    $previousWindowHandle = Wait-ForVisibleApplicationWindow -Process $previousProcess
    $previousProcessId = $previousProcess.Id
    $report.checks.runningPreviousVersion = [ordered]@{
      processId = $previousProcessId
      sessionId = $previousProcess.SessionId
      windowHandle = $previousWindowHandle
      executablePath = $previousState.ExecutablePath
      visibleWindowVerified = $true
    }

    Write-Host "验证运行中 $PreviousVersion -> $ExpectedVersion 管理员 /qn 覆盖升级..."
    Invoke-Install `
      -ResolvedMsi $resolvedMsi `
      -LogPath (Join-Path $resolvedEvidence '02-upgrade-current.log') `
      -DisableLaunch
    $currentState = Assert-InstalledState -Version $ExpectedVersion
    $lastExecutablePath = $currentState.ExecutablePath
    if ($currentState.ProductCode -eq $previousState.ProductCode) {
      throw '覆盖升级后 ProductCode 未变化。'
    }
    if (-not $currentState.ExecutablePath.Equals(
        $previousState.ExecutablePath,
        [StringComparison]::OrdinalIgnoreCase
      )) {
      throw "覆盖升级改变了正式安装路径：旧=$($previousState.ExecutablePath)，新=$($currentState.ExecutablePath)。"
    }
    if ($currentState.ExecutableSha256 -eq $previousState.ExecutableSha256) {
      throw '覆盖升级后主程序 SHA256 未变化，疑似仍在运行上一版文件。'
    }
    if (@(Get-InstalledProducts | Where-Object { $_.PSChildName -eq $previousState.ProductCode }).Count -ne 0) {
      throw '覆盖升级后旧 ProductCode 仍存在。'
    }
    $previousProcess.Refresh()
    if (-not $previousProcess.HasExited) {
      throw "覆盖升级后旧版进程 $previousProcessId 仍在运行，MSI CloseApplication 未生效。"
    }
    Assert-NoHualiProcesses -Stage "$ExpectedVersion 禁止启动覆盖升级"
    $report.checks.upgrade = [ordered]@{
      previousProductCode = $previousState.ProductCode
      currentProductCode = $currentState.ProductCode
      executablePathStable = $true
      previousExecutableSha256 = $previousState.ExecutableSha256
      currentExecutableSha256 = $currentState.ExecutableSha256
      singleProductRegistration = $true
      runningPreviousProcessClosed = $true
      currentProcessNotAutoStarted = $true
    }
  } else {
    Write-Host "验证 $ExpectedVersion 管理员 /qn 空机安装..."
    Invoke-Install `
      -ResolvedMsi $resolvedMsi `
      -LogPath (Join-Path $resolvedEvidence '01-current-install.log') `
      -DisableLaunch
    $currentState = Assert-InstalledState -Version $ExpectedVersion
    $lastExecutablePath = $currentState.ExecutablePath
    Assert-NoHualiProcesses -Stage "$ExpectedVersion 禁止启动安装"
    $report.checks.cleanInstall = $currentState
  }

  if ($RequireDiagnosticLogCleanup) {
    $diagnosticCleanupFixture = New-DesktopDiagnosticCleanupFixture
  }

  $installedBeforeFirstUninstall = Assert-InstalledState -Version $ExpectedVersion
  Write-Host '验证 Windows 真协议唤起与原生登录回调交付...'
  $report.checks.desktopAuthProtocolCallback = Invoke-DesktopAuthProtocolCallbackSmoke `
    -ExpectedExecutablePath $installedBeforeFirstUninstall.ExecutablePath `
    -ExpectedAppVersion $ExpectedVersion `
    -DiagnosticEvidenceDirectory $resolvedEvidence `
    -RequireRawDiagnostics:$RequireRawAuthDiagnostics `
    -DiagnosticOnly:$DiagnosticOnly
  if ($RequireDiagnosticLogCleanup) {
    Assert-DesktopDiagnosticCleanup -Fixture $diagnosticCleanupFixture
    $report.checks.diagnosticLogCleanup = [ordered]@{
      primaryRemoved = $true
      rotatedRemoved = $true
      unrelatedFileRetained = $true
      newDiagnosticLogNotCreated = $true
    }
  }
  Stop-HualiProcesses
  Assert-NoHualiProcesses -Stage '登录回调协议冒烟测试'
  Invoke-UninstallProduct `
    -ProductCode $installedBeforeFirstUninstall.ProductCode `
    -LogPath (Join-Path $resolvedEvidence '03-upgraded-uninstall.log')
  Assert-UninstalledState `
    -Stage '覆盖升级包静默卸载' `
    -FormerExecutablePath $installedBeforeFirstUninstall.ExecutablePath
  $report.checks.upgradedUninstall = [ordered]@{
    productRegistrationRemoved = $true
    markerRemoved = $true
    runValueRemoved = $true
    executableRemoved = $true
    installDirectoryRemoved = $true
    processRemoved = $true
  }

  if ($ValidateDefaultLaunch) {
    $currentSessionId = [Diagnostics.Process]::GetCurrentProcess().SessionId
    $interactiveSessions = @(Get-InteractiveExplorerSessions)
    if ($interactiveSessions.Count -gt 0 -and $currentSessionId -notin $interactiveSessions) {
      throw "当前验收进程在会话 $currentSessionId，Explorer 位于其他会话；无法可靠截图和识别默认启动窗口。"
    }

    Write-Host '验证正式默认的安装后立即启动路径...'
    $launchLogLineCount = Get-LaunchLogLineCount
    Invoke-Install `
      -ResolvedMsi $resolvedMsi `
      -LogPath (Join-Path $resolvedEvidence '04-current-default-launch-install.log')
    $defaultLaunchState = Assert-InstalledState -Version $ExpectedVersion
    $lastExecutablePath = $defaultLaunchState.ExecutablePath
    if ($currentSessionId -in $interactiveSessions) {
      $launchedProcess = Wait-ForDefaultLaunch `
        -ExecutablePath $defaultLaunchState.ExecutablePath `
        -InteractiveSessions $interactiveSessions
      $launchLogLine = Wait-ForNewLaunchLogLine `
        -StartingLineCount $launchLogLineCount `
        -Pattern 'STARTED account='
      $report.checks.defaultLaunch = [ordered]@{
        status = 'interactive-launch-verified'
        processId = $launchedProcess.Id
        sessionId = $launchedProcess.SessionId
        windowHandle = [HualiDeploymentSmokeNative]::FindVisibleApplicationWindow($launchedProcess.Id).ToInt64()
        executablePath = $defaultLaunchState.ExecutablePath
        helperLogLine = $launchLogLine
        interactiveSessionVerified = $true
        hklmRunFallbackVerified = $true
        manualInteractiveGateRequired = $false
      }
      Stop-HualiProcesses
    } else {
      $launchLogLine = Wait-ForNewLaunchLogLine `
        -StartingLineCount $launchLogLineCount `
        -Pattern 'SKIP no-interactive-user'
      Assert-NoHualiProcesses -Stage '无交互会话默认安装'
      $report.checks.defaultLaunch = [ordered]@{
        status = 'non-interactive-fallback-verified'
        executablePath = $defaultLaunchState.ExecutablePath
        helperLogLine = $launchLogLine
        interactiveSessionVerified = $false
        helperNoInteractiveFallbackVerified = $true
        hklmRunFallbackVerified = $true
        processNotStarted = $true
        manualInteractiveGateRequired = $true
        manualCommand = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Test-HualiAISilentDeployment.ps1 -MsiPath .\<${ExpectedVersion}.msi> -ExpectedVersion $ExpectedVersion -ValidateDefaultLaunch -RequireInteractiveDefaultLaunch"
      }
      if ($RequireInteractiveDefaultLaunch) {
        throw '当前 Windows runner 没有 Explorer 交互桌面；默认启动的无用户回退路径已验证，但本次要求的交互真机门禁明确失败。'
      }
      Write-Warning '当前 runner 无 Explorer：已验证辅助程序 SKIP 日志、不误启动进程与 HKLM Run 兜底；仍需在真实登录用户的 Windows 上执行交互默认启动门禁。'
    }
  } elseif ($RunVisualSmoke) {
    Invoke-Install `
      -ResolvedMsi $resolvedMsi `
      -LogPath (Join-Path $resolvedEvidence '04-current-visual-install.log') `
      -DisableLaunch
    $visualInstallState = Assert-InstalledState -Version $ExpectedVersion
    $lastExecutablePath = $visualInstallState.ExecutablePath
  }

  if ($RunVisualSmoke) {
    $visualInteractiveSessions = @(Get-InteractiveExplorerSessions)
    $visualSessionId = [Diagnostics.Process]::GetCurrentProcess().SessionId
    if ($visualSessionId -notin $visualInteractiveSessions) {
      throw "当前 Windows runner 会话 $visualSessionId 没有 Explorer 交互桌面，无法执行真实窗口截图、鼠标交互和 DPI 视觉门禁；本次发布验收明确失败。"
    }
    Assert-NoHualiProcesses -Stage '真实窗口视觉冒烟测试启动前'
    $visualSmokeScript = Join-Path $PSScriptRoot 'Test-HualiAIWindowsVisualSmoke.ps1'
    if (-not (Test-Path -LiteralPath $visualSmokeScript -PathType Leaf)) {
      throw "缺少 Windows 视觉冒烟测试脚本：$visualSmokeScript"
    }
    & $visualSmokeScript `
      -ExecutablePath $lastExecutablePath `
      -OutputDirectory $resolvedVisualEvidence
    Assert-NoHualiProcesses -Stage '真实窗口视觉冒烟测试结束'
    $report.checks.visualSmoke = [ordered]@{
      outputDirectory = $resolvedVisualEvidence
      completed = $true
    }
  }

  $finalProducts = @(Get-InstalledProducts)
  if ($finalProducts.Count -eq 1) {
    Invoke-UninstallProduct `
      -ProductCode ([string]$finalProducts[0].PSChildName) `
      -LogPath (Join-Path $resolvedEvidence '05-final-uninstall.log')
  } elseif ($finalProducts.Count -ne 0) {
    throw "最终卸载前产品登记数量错误：$($finalProducts.Count)。"
  }
  Assert-UninstalledState -Stage '最终静默卸载' -FormerExecutablePath $lastExecutablePath
  $report.checks.finalUninstall = [ordered]@{
    productRegistrationRemoved = $true
    markerRemoved = $true
    runValueRemoved = $true
    executableRemoved = $true
    installDirectoryRemoved = $true
    processRemoved = $true
  }
  $report.ok = $true
  if ($DiagnosticOnly) {
    Write-Host '诊断包管理员 /qn 覆盖升级、真协议回调与原始日志落盘验证全部通过。'
  } else {
    Write-Host '管理员 /qn 覆盖升级、本 runner 可执行的默认启动路径与完整卸载验证全部通过。'
  }
} catch {
  $primaryFailure = $_
  $report.failure = $_.Exception.Message
} finally {
  if ($diagnosticCleanupFixture) {
    foreach ($fixturePath in @(
      $diagnosticCleanupFixture.primaryPath,
      $diagnosticCleanupFixture.rotatedPath,
      $diagnosticCleanupFixture.retainedPath
    )) {
      Remove-Item -LiteralPath $fixturePath -Force -ErrorAction SilentlyContinue
    }
  }
  if ($cleanupAuthorized) {
    try {
      Stop-HualiProcesses
      $cleanupIndex = 0
      foreach ($product in Get-InstalledProducts) {
        $cleanupIndex++
        Invoke-UninstallProduct `
          -ProductCode ([string]$product.PSChildName) `
          -LogPath (Join-Path $resolvedEvidence ("99-emergency-cleanup-$cleanupIndex.log"))
      }
      Assert-UninstalledState -Stage '测试清理' -FormerExecutablePath $lastExecutablePath
      $report.cleanup = [ordered]@{ completed = $true }
    } catch {
      $cleanupFailure = $_
      $report.ok = $false
      if (-not $report.failure) {
        $report.failure = $_.Exception.Message
      }
      $report.cleanup = [ordered]@{
        completed = $false
        failure = $_.Exception.Message
      }
    }
  }

  if (Test-Path -LiteralPath $LaunchLogPath -PathType Leaf) {
    Copy-Item -LiteralPath $LaunchLogPath -Destination (Join-Path $resolvedEvidence 'launch-after-install.log') -Force
  }
  $report.completedAt = [DateTime]::UtcNow.ToString('o')
  $report | ConvertTo-Json -Depth 8 |
    Set-Content -LiteralPath (Join-Path $resolvedEvidence 'deployment-smoke-report.json') -Encoding UTF8
}

if ($primaryFailure) {
  throw $primaryFailure
}
if ($cleanupFailure) {
  throw $cleanupFailure
}
