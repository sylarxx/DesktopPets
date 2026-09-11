[CmdletBinding()]
param(
  [ValidateSet('prod', 'main', 'intranet')]
  [string]$Mode = 'intranet',

  [switch]$AllowUnsigned,

  [switch]$SkipNpmCi,

  [string]$TimestampUrl = $env:WINDOWS_TIMESTAMP_URL
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ProductName = '华力AI桌面助手'
$MainBinaryName = 'HualiAIDesktopAssistant.exe'
$UpgradeCode = '07C9B303-2B8E-48E4-AB16-6EB2FB87DF13'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$EnterpriseConfig = Join-Path $Root 'src-tauri\tauri.enterprise-msi.conf.json'
$GeneratedConfig = Join-Path $Root 'src-tauri\tauri.enterprise-msi.generated.conf.json'
$MsiOutputDirectory = Join-Path $Root 'src-tauri\target\release\bundle\msi'
$ApplicationExe = Join-Path $Root "src-tauri\target\release\$MainBinaryName"

function Assert-Command {
  param([Parameter(Mandatory = $true)][string]$Name)

  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "缺少构建依赖：$Name"
  }
}

function Get-CargoPackageVersion {
  param([Parameter(Mandatory = $true)][string]$CargoTomlPath)

  $content = Get-Content -LiteralPath $CargoTomlPath -Raw -Encoding UTF8
  $match = [regex]::Match($content, '(?ms)^\[package\]\s*.*?^version\s*=\s*"([^"]+)"')
  if (-not $match.Success) {
    throw "无法从 $CargoTomlPath 读取 package.version"
  }
  return $match.Groups[1].Value
}

function Add-OrSetJsonProperty {
  param(
    [Parameter(Mandatory = $true)]$Target,
    [Parameter(Mandatory = $true)][string]$Name,
    [Parameter(Mandatory = $true)]$Value
  )

  if ($Target.PSObject.Properties.Name -contains $Name) {
    $Target.$Name = $Value
  } else {
    $Target | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
  }
}

if ($env:OS -ne 'Windows_NT') {
  throw 'MSI 必须在 Windows x64 构建机上生成。'
}
if (-not [Environment]::Is64BitOperatingSystem) {
  throw '企业 MSI 仅支持 Windows x64 构建机。'
}
$nativeArchitecture = if ($env:PROCESSOR_ARCHITEW6432) {
  $env:PROCESSOR_ARCHITEW6432
} else {
  $env:PROCESSOR_ARCHITECTURE
}
if ($nativeArchitecture -ne 'AMD64') {
  throw "企业产物固定为 x64，请使用 AMD64 Windows 构建机；当前为 $nativeArchitecture。"
}

Assert-Command -Name 'node'
Assert-Command -Name 'npm'
Assert-Command -Name 'cargo'
Assert-Command -Name 'rustc'
Assert-Command -Name 'cscript.exe'
Assert-Command -Name 'powershell.exe'

$rustcVersionInfo = @(& rustc -vV)
if ($LASTEXITCODE -ne 0) {
  throw "rustc -vV 失败，退出码：$LASTEXITCODE"
}
$rustHost = $rustcVersionInfo | Where-Object { $_ -like 'host:*' } | Select-Object -First 1
if ($rustHost -ne 'host: x86_64-pc-windows-msvc') {
  throw "Rust 必须使用 x86_64-pc-windows-msvc toolchain，当前为 '$rustHost'。"
}

$PackageVersion = (Get-Content -LiteralPath (Join-Path $Root 'package.json') -Raw -Encoding UTF8 | ConvertFrom-Json).version
$TauriVersion = (Get-Content -LiteralPath (Join-Path $Root 'src-tauri\tauri.conf.json') -Raw -Encoding UTF8 | ConvertFrom-Json).version
$CargoVersion = Get-CargoPackageVersion -CargoTomlPath (Join-Path $Root 'src-tauri\Cargo.toml')

if ($PackageVersion -notmatch '^\d+\.\d+\.\d+$') {
  throw "MSI 版本必须是三段数字版本号，当前 package.json 为 $PackageVersion"
}
if (($PackageVersion -ne $TauriVersion) -or ($PackageVersion -ne $CargoVersion)) {
  throw "版本号不一致：package.json=$PackageVersion, tauri.conf.json=$TauriVersion, Cargo.toml=$CargoVersion"
}

$CertificateThumbprint = [string]$env:WINDOWS_CERTIFICATE_THUMBPRINT
$CertificateThumbprint = ($CertificateThumbprint -replace '\s', '').ToUpperInvariant()
if (-not $TimestampUrl) {
  $TimestampUrl = 'http://timestamp.digicert.com'
}

$BuildConfig = $EnterpriseConfig
$SigningDescription = '未签名测试包'

if ($CertificateThumbprint) {
  $certificate = @(
    Get-ChildItem -Path 'Cert:\CurrentUser\My' -ErrorAction SilentlyContinue
    Get-ChildItem -Path 'Cert:\LocalMachine\My' -ErrorAction SilentlyContinue
  ) | Where-Object {
    (($_.Thumbprint -replace '\s', '').ToUpperInvariant() -eq $CertificateThumbprint) -and $_.HasPrivateKey
  } | Select-Object -First 1

  if (-not $certificate) {
    throw "没有找到带私钥的代码签名证书：$CertificateThumbprint"
  }
  $now = Get-Date
  if (($certificate.NotBefore -gt $now) -or ($certificate.NotAfter -le $now)) {
    throw "代码签名证书当前不在有效期内：$($certificate.NotBefore) - $($certificate.NotAfter)"
  }
  $codeSigningEku = $certificate.EnhancedKeyUsageList | Where-Object {
    $_.ObjectId.Value -eq '1.3.6.1.5.5.7.3.3'
  }
  if (-not $codeSigningEku) {
    throw "证书不包含代码签名 EKU：$CertificateThumbprint"
  }

  $config = Get-Content -LiteralPath $EnterpriseConfig -Raw -Encoding UTF8 | ConvertFrom-Json
  Add-OrSetJsonProperty -Target $config.bundle.windows -Name 'certificateThumbprint' -Value $CertificateThumbprint
  Add-OrSetJsonProperty -Target $config.bundle.windows -Name 'digestAlgorithm' -Value 'sha256'
  Add-OrSetJsonProperty -Target $config.bundle.windows -Name 'timestampUrl' -Value $TimestampUrl
  $generatedJson = $config | ConvertTo-Json -Depth 20
  $utf8WithoutBom = New-Object Text.UTF8Encoding($false)
  [IO.File]::WriteAllText($GeneratedConfig, $generatedJson, $utf8WithoutBom)
  $BuildConfig = $GeneratedConfig
  $SigningDescription = "签名证书：$($certificate.Subject) [$CertificateThumbprint]"
} elseif (-not $AllowUnsigned) {
  throw '生产企业包必须签名。请设置 WINDOWS_CERTIFICATE_THUMBPRINT，或仅测试时显式传入 -AllowUnsigned。'
}

Push-Location $Root
try {
  if (-not $SkipNpmCi) {
    Write-Host '正在按 package-lock.json 安装前端依赖...'
    & npm ci
    if ($LASTEXITCODE -ne 0) {
      throw "npm ci 失败，退出码：$LASTEXITCODE"
    }
  }

  $env:BUILD_MODE = $Mode
  $buildStarted = (Get-Date).AddSeconds(-3)
  $tauriArguments = @(
    'run', 'tauri', 'build', '--',
    '--bundles', 'msi',
    '--config', $BuildConfig,
    '--ci'
  )
  if ($AllowUnsigned -and -not $CertificateThumbprint) {
    $tauriArguments += '--no-sign'
  }

  Write-Host "正在构建 $Mode 环境的 Windows x64 企业 MSI..."
  & npm @tauriArguments
  if ($LASTEXITCODE -ne 0) {
    throw "Tauri MSI 构建失败，退出码：$LASTEXITCODE"
  }

  if (-not (Test-Path -LiteralPath $MsiOutputDirectory)) {
    throw "没有生成 MSI 输出目录：$MsiOutputDirectory"
  }
  if (-not (Test-Path -LiteralPath $ApplicationExe)) {
    throw "没有生成 Windows 主程序：$ApplicationExe"
  }

  $builtMsi = Get-ChildItem -LiteralPath $MsiOutputDirectory -Filter '*.msi' -File |
    Where-Object { $_.LastWriteTime -ge $buildStarted } |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
  if (-not $builtMsi) {
    throw "没有找到本次生成的 MSI：$MsiOutputDirectory"
  }

  $appSignature = Get-AuthenticodeSignature -LiteralPath $ApplicationExe
  $msiSignature = Get-AuthenticodeSignature -LiteralPath $builtMsi.FullName
  if (-not $AllowUnsigned) {
    if ($appSignature.Status -ne 'Valid') {
      throw "主程序签名无效：$($appSignature.Status)"
    }
    if ($msiSignature.Status -ne 'Valid') {
      throw "MSI 签名无效：$($msiSignature.Status)"
    }
  }

  $artifactsDirectory = Join-Path $Root 'artifacts'
  $stagingDirectory = Join-Path $artifactsDirectory "windows-msi-$Mode"
  $outputName = "Huali-AI-Desktop-Assistant_${PackageVersion}_${Mode}_x64-enterprise.msi"
  $outputMsi = Join-Path $stagingDirectory $outputName
  $zipPath = Join-Path $artifactsDirectory "huali-ai-mascot-${PackageVersion}-${Mode}-windows-enterprise-msi-x64.zip"

  if (Test-Path -LiteralPath $stagingDirectory) {
    Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
  }
  New-Item -ItemType Directory -Path $stagingDirectory -Force | Out-Null

  Copy-Item -LiteralPath $builtMsi.FullName -Destination $outputMsi
  Copy-Item -LiteralPath (Join-Path $Root 'scripts\windows-msi\Install-HualiAI.ps1') -Destination $stagingDirectory
  Copy-Item -LiteralPath (Join-Path $Root 'scripts\windows-msi\Uninstall-HualiAI.ps1') -Destination $stagingDirectory
  Copy-Item -LiteralPath (Join-Path $Root 'scripts\windows-msi\Test-HualiAIMsi.ps1') -Destination $stagingDirectory
  Copy-Item -LiteralPath (Join-Path $Root 'scripts\windows-msi\Test-HualiAISilentDeployment.ps1') -Destination $stagingDirectory
  Copy-Item -LiteralPath (Join-Path $Root 'scripts\windows-msi\Test-HualiAIWindowsVisualSmoke.ps1') -Destination $stagingDirectory
  Copy-Item -LiteralPath (Join-Path $Root 'scripts\windows-msi\Test-HualiAIMessageDrag.ps1') -Destination $stagingDirectory
  Copy-Item -LiteralPath (Join-Path $Root 'scripts\windows-msi\平台部署说明.txt') -Destination (Join-Path $stagingDirectory 'README.txt')

  $msiHash = (Get-FileHash -LiteralPath $outputMsi -Algorithm SHA256).Hash.ToLowerInvariant()
  $appHash = (Get-FileHash -LiteralPath $ApplicationExe -Algorithm SHA256).Hash.ToLowerInvariant()
  "$msiHash  $outputName" | Set-Content -LiteralPath (Join-Path $stagingDirectory 'SHA256.txt') -Encoding ASCII
  "$appHash  $MainBinaryName" | Set-Content -LiteralPath (Join-Path $stagingDirectory 'APPLICATION-SHA256.txt') -Encoding ASCII
  @(
    $SigningDescription
    "主程序签名状态：$($appSignature.Status)"
    "MSI 签名状态：$($msiSignature.Status)"
    "固定 UpgradeCode：$UpgradeCode"
  ) | Set-Content -LiteralPath (Join-Path $stagingDirectory 'SIGNATURES.txt') -Encoding UTF8

  $testArguments = @(
    '-NoProfile', '-ExecutionPolicy', 'Bypass',
    '-File', (Join-Path $Root 'scripts\windows-msi\Test-HualiAIMsi.ps1'),
    '-MsiPath', $outputMsi,
    '-ExpectedVersion', $PackageVersion,
    '-ExpectedUpgradeCode', $UpgradeCode
  )
  if ($AllowUnsigned) {
    $testArguments += '-AllowUnsigned'
  }
  & powershell.exe @testArguments
  if ($LASTEXITCODE -ne 0) {
    throw "MSI 结构验收失败，退出码：$LASTEXITCODE"
  }

  if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
  }
  Compress-Archive -Path (Join-Path $stagingDirectory '*') -DestinationPath $zipPath -CompressionLevel Optimal

  Write-Host ''
  Write-Host 'Windows 企业 MSI 构建完成：'
  Write-Host "  MSI：$outputMsi"
  Write-Host "  管理员分发包：$zipPath"
  Write-Host '  静默部署：powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Install-HualiAI.ps1 -MsiPath .\安装包.msi'
  Write-Host '  直接 MSI：msiexec.exe /i 安装包.msi /qn /norestart REBOOT=ReallySuppress /L*v C:\Windows\Temp\HualiAI-install.log'
} finally {
  Pop-Location
  if (Test-Path -LiteralPath $GeneratedConfig) {
    Remove-Item -LiteralPath $GeneratedConfig -Force
  }
}
