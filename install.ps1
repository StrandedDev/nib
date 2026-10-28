# nib installer for Windows x64.
#
#   irm https://raw.githubusercontent.com/StrandedDev/nib/main/install.ps1 | iex
#
# Downloads the latest nib MSI, checks it against SHA256SUMS, and installs it
# silently with msiexec (per-machine into Program Files, added to PATH).
# Re-running updates in place. Uninstall from "Add or remove programs".
# The temporary download folder is always deleted.

& {
    Set-StrictMode -Version 3.0
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest is very slow with the progress bar

    $Repo = 'StrandedDev/nib'

    # Windows PowerShell 5.1 may default to TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    if (-not [Environment]::Is64BitOperatingSystem) {
        throw 'nib: only 64-bit Windows is supported.'
    }

    $headers = @{ 'User-Agent' = 'nib-installer'; 'Accept' = 'application/vnd.github+json' }
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers $headers
    $tag = [string]$release.tag_name
    $version = $tag -replace '^v', ''
    $msiName = "nib-$version-x86_64.msi"

    $assets = @($release.assets)
    $msiAsset = $assets | Where-Object { $_.name -eq $msiName } | Select-Object -First 1
    $sumsAsset = $assets | Where-Object { $_.name -eq 'SHA256SUMS' } | Select-Object -First 1
    if (-not $msiAsset -or -not $sumsAsset) {
        throw "nib: release $tag has no $msiName or SHA256SUMS."
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("nib-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $msi = Join-Path $tmp $msiName
        $sums = Join-Path $tmp 'SHA256SUMS'

        Write-Host "nib: downloading $msiName"
        Invoke-WebRequest -Uri $sumsAsset.browser_download_url -OutFile $sums -UseBasicParsing
        Invoke-WebRequest -Uri $msiAsset.browser_download_url -OutFile $msi -UseBasicParsing

        $expected = $null
        foreach ($line in Get-Content -LiteralPath $sums) {
            $parts = $line -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $msiName) {
                $expected = $parts[0].ToLowerInvariant()
                break
            }
        }
        if (-not $expected) {
            throw "nib: $msiName is not listed in SHA256SUMS."
        }
        $actual = (Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            throw "nib: checksum mismatch for $msiName (expected $expected, got $actual)."
        }
        Write-Host 'nib: checksum ok'

        # Per-machine install needs elevation; /qn cannot show a UAC prompt itself.
        $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
            [Security.Principal.WindowsBuiltInRole]::Administrator)
        $msiArgs = @('/i', "`"$msi`"", '/qn', '/norestart')
        Write-Host "nib: installing nib $version"
        if ($isAdmin) {
            $proc = Start-Process -FilePath 'msiexec.exe' -ArgumentList $msiArgs -Wait -PassThru
        } else {
            Write-Host 'nib: approve the administrator prompt to continue'
            $proc = Start-Process -FilePath 'msiexec.exe' -ArgumentList $msiArgs -Wait -PassThru -Verb RunAs
        }
        switch ($proc.ExitCode) {
            0 { }
            3010 { Write-Host 'nib: installed; Windows asks for a restart to finish.' }
            1602 { throw 'nib: installation cancelled.' }
            default { throw "nib: msiexec failed with exit code $($proc.ExitCode)." }
        }
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    # ProgramW6432 is the 64-bit Program Files even from 32-bit PowerShell.
    $programFiles = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles }
    $installDir = Join-Path $programFiles 'nib\bin'
    $exe = Join-Path $installDir 'nib.exe'
    if (-not (Test-Path -LiteralPath $exe)) {
        throw "nib: installed, but $exe was not found."
    }
    & $exe --version

    # Make nib usable in this session too; new terminals get it from the system PATH.
    if (-not (($env:Path -split ';') -contains $installDir)) {
        $env:Path = "$env:Path;$installDir"
    }
    Write-Host 'nib: done. Open a new terminal if another one cannot find nib.'
}
