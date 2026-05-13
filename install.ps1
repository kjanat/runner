#!/usr/bin/env pwsh

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

$Repo = "kjanat/runner"

function Show-Usage {
	@'
Install runner binaries from GitHub Releases.

Usage:
  install.ps1 [X.Y.Z|vX.Y.Z]

Arguments:
  X.Y.Z|vX.Y.Z  Optional release tag. If omitted, installs latest release.

Environment:
  RUNNER_VERSION      Release tag override (e.g. 0.1.0 or v0.1.0)
  RUNNER_INSTALL_DIR  Destination directory (highest precedence)
  XDG_BIN_HOME        Destination directory fallback before ~/.local/bin on non-Windows

Defaults:
  Windows             %LOCALAPPDATA%\Programs\runner\bin
  Other platforms     $HOME/.local/bin
'@
}

function Write-Step {
	param([string] $Message)
	Write-Output "==> $Message"
}

function Write-Item {
	param([string] $Message)
	Write-Output "  - $Message"
}

function Get-RequiredCommand {
	param([string] $Name)

	$commands = @(Get-Command -Name $Name -CommandType Application -ErrorAction SilentlyContinue)
	if ($commands.Count -eq 0) {
		throw "required command not found: $Name"
	}

	return $commands[0].Source
}

function Get-CurrentArch {
	try {
		return [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
	} catch {
		# Fall back below for older Windows PowerShell runtimes.
	}

	if ($env:PROCESSOR_ARCHITECTURE) {
		return $env:PROCESSOR_ARCHITECTURE
	}

	$uname = @(Get-Command -Name uname -CommandType Application -ErrorAction SilentlyContinue)
	if ($uname.Count -gt 0) {
		return (& $uname[0].Source -m).Trim()
	}

	throw "unsupported architecture"
}

function Resolve-Target {
	param([string] $Arch)

	$arch = $Arch.ToLowerInvariant()

	if ($IsWindows) {
		switch ($arch) {
			{ $_ -in @("x64", "amd64", "x86_64") } { return "x86_64-pc-windows-msvc" }
			{ $_ -in @("arm64", "aarch64") } { return "aarch64-pc-windows-msvc" }
			{ $_ -in @("x86", "i386", "i686") } { return "i686-pc-windows-msvc" }
		}
	} elseif ($IsMacOS) {
		switch ($arch) {
			{ $_ -in @("x64", "amd64", "x86_64") } { return "x86_64-apple-darwin" }
			{ $_ -in @("arm64", "aarch64") } { return "aarch64-apple-darwin" }
		}
	} elseif ($IsLinux) {
		switch ($arch) {
			{ $_ -in @("x64", "amd64", "x86_64") } { return "x86_64-unknown-linux-musl" }
			{ $_ -in @("arm64", "aarch64") } { return "aarch64-unknown-linux-musl" }
			{ $_ -in @("arm", "armv7l") } { return "armv7-unknown-linux-gnueabihf" }
		}
	}

	throw "unsupported platform or architecture: $Arch"
}

function Resolve-InstallDir {
	if ($env:RUNNER_INSTALL_DIR) {
		return $env:RUNNER_INSTALL_DIR
	}

	if ($IsWindows) {
		if ($env:LOCALAPPDATA) {
			return (Join-Path $env:LOCALAPPDATA "Programs\runner\bin")
		}
		if ($HOME) {
			return (Join-Path $HOME "AppData\Local\Programs\runner\bin")
		}
		throw "LOCALAPPDATA or HOME is required"
	}

	if ($env:XDG_BIN_HOME) {
		return $env:XDG_BIN_HOME
	}
	if ($HOME) {
		return (Join-Path $HOME ".local/bin")
	}

	throw "HOME is required"
}

function Invoke-Download {
	param(
		[string] $Uri,
		[string] $OutFile
	)

	for ($attempt = 1; $attempt -le 3; $attempt++) {
		try {
			Invoke-WebRequest -Uri $Uri -OutFile $OutFile -UseBasicParsing
			return
		} catch {
			if ($attempt -eq 3) {
				throw
			}
			Start-Sleep -Seconds 1
		}
	}
}

function Resolve-LatestVersion {
	try {
		$response = Invoke-WebRequest `
			-Uri "https://api.github.com/repos/$Repo/releases/latest" `
			-Headers @{ Accept = "application/vnd.github+json" } `
			-UseBasicParsing
		$release = $response.Content | ConvertFrom-Json
		if ($release.tag_name) {
			return $release.tag_name
		}
	} catch {
		throw "failed to resolve latest release version: $($_.Exception.Message)"
	}

	throw "failed to resolve latest release version"
}

function Test-Checksum {
	param(
		[string] $ArchivePath,
		[string] $ChecksumPath
	)

	$line = (Get-Content -LiteralPath $ChecksumPath -TotalCount 1).Trim()
	$expected = @($line -split "\s+")[0].ToLowerInvariant()
	if ($expected -notmatch "^[0-9a-f]{64}$") {
		throw "invalid checksum file: $ChecksumPath"
	}

	$actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $ArchivePath).Hash.ToLowerInvariant()
	if ($actual -ne $expected) {
		throw "checksum verification failed for $ArchivePath"
	}
}

function Get-PathWithoutTrailingSeparator {
	param([string] $Path)

	if (-not $Path) {
		return ""
	}

	[char[]] $trimChars = @([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
	return $Path.TrimEnd($trimChars)
}

function Test-PathEntry {
	param([string] $Directory)

	$pathValue = [string] $env:PATH
	$comparison = [System.StringComparison]::Ordinal
	if ($IsWindows) {
		$comparison = [System.StringComparison]::OrdinalIgnoreCase
	}

	foreach ($entry in $pathValue -split [System.IO.Path]::PathSeparator) {
		if ([string]::Equals((Get-PathWithoutTrailingSeparator -Path $entry), (Get-PathWithoutTrailingSeparator -Path $Directory), $comparison)) {
			return $true
		}
	}

	return $false
}

function Install-Runner {
	param([string[]] $ScriptArgs)

	if ($ScriptArgs.Count -eq 1 -and $ScriptArgs[0] -in @("-h", "--help")) {
		Show-Usage
		return
	}

	if ($ScriptArgs.Count -gt 1) {
		Show-Usage | ForEach-Object { [Console]::Error.WriteLine($_) }
		exit 1
	}

	try {
		[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
	} catch {
		# Older/non-Windows runtimes may not expose ServicePointManager.
	}

	if (-not ($IsWindows -or $IsLinux -or $IsMacOS)) {
		throw "unsupported operating system"
	}

	$arch = Get-CurrentArch
	$target = Resolve-Target -Arch $arch
	$installDir = Resolve-InstallDir
	$tar = Get-RequiredCommand -Name "tar"

	$version = $env:RUNNER_VERSION
	if (-not $version -and $ScriptArgs.Count -eq 1) {
		$version = $ScriptArgs[0]
	}
	if (-not $version) {
		$version = Resolve-LatestVersion
	}
	if (-not $version.StartsWith("v", [System.StringComparison]::Ordinal)) {
		$version = "v$version"
	}

	$asset = "runner-$version-$target.tar.gz"
	$checksumAsset = "runner-$version-$target.sha256"
	$baseUrl = "https://github.com/$Repo/releases/download/$version"
	$tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("runner-install-" + [System.Guid]::NewGuid().ToString("N"))

	New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

	try {
		$archivePath = Join-Path $tmpDir $asset
		$checksumPath = Join-Path $tmpDir $checksumAsset

		Write-Step "Downloading release assets"
		Write-Item "archive: $asset"
		Invoke-Download -Uri "$baseUrl/$asset" -OutFile $archivePath
		Invoke-Download -Uri "$baseUrl/$checksumAsset" -OutFile $checksumPath

		Test-Checksum -ArchivePath $archivePath -ChecksumPath $checksumPath

		& $tar -xzf $archivePath -C $tmpDir
		if ($LASTEXITCODE -ne 0) {
			throw "failed to extract $asset"
		}

		$extension = ""
		if ($IsWindows) {
			$extension = ".exe"
		}

		$binaries = @("runner$extension", "run$extension")
		foreach ($binary in $binaries) {
			$binaryPath = Join-Path $tmpDir $binary
			if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
				throw "missing binary in archive: $binary"
			}
		}

		New-Item -ItemType Directory -Path $installDir -Force | Out-Null
		foreach ($binary in $binaries) {
			Copy-Item -LiteralPath (Join-Path $tmpDir $binary) -Destination (Join-Path $installDir $binary) -Force
		}

		if (-not $IsWindows) {
			$chmod = Get-RequiredCommand -Name "chmod"
			& $chmod 755 (Join-Path $installDir "runner") (Join-Path $installDir "run")
			if ($LASTEXITCODE -ne 0) {
				throw "failed to mark installed binaries executable"
			}
		}

		Write-Step "Installation complete"
		Write-Item "location: $installDir"

		$expectedRunner = Join-Path $installDir "runner$extension"
		try {
			$installedVersion = & $expectedRunner -V
			Write-Item "version: $installedVersion"
		} catch {
			Write-Item "warning: failed to execute $expectedRunner -V"
		}

		$resolvedInstallDir = (Resolve-Path -LiteralPath $installDir).ProviderPath
		if (-not (Test-PathEntry -Directory $resolvedInstallDir)) {
			Write-Item "PATH: add $resolvedInstallDir to your PATH"
		}

		$resolvedRunner = @(Get-Command -Name "runner" -CommandType Application -ErrorAction SilentlyContinue)
		if ($resolvedRunner.Count -gt 0) {
			$comparison = [System.StringComparison]::Ordinal
			if ($IsWindows) {
				$comparison = [System.StringComparison]::OrdinalIgnoreCase
			}
			if (-not [string]::Equals($resolvedRunner[0].Source, $expectedRunner, $comparison)) {
				Write-Item "refresh shell: restart the shell if needed"
			}
		}
	} finally {
		if (Test-Path -LiteralPath $tmpDir) {
			Remove-Item -LiteralPath $tmpDir -Recurse -Force
		}
	}
}

try {
	Install-Runner -ScriptArgs $args
} catch {
	[Console]::Error.WriteLine("error: $($_.Exception.Message)")
	exit 1
}
