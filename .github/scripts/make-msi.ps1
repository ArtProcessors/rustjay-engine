# Builds a per-app .msi from a dist folder using the WiX dotnet tool.
# The MSI installs the folder to Program Files\<Name> and adds a Start-menu shortcut.
param(
    [Parameter(Mandatory)] [string]$Name,       # display name, e.g. "CuePool"
    [Parameter(Mandatory)] [string]$Version,    # numeric x.y.z
    [Parameter(Mandatory)] [string]$SourceDir,  # folder whose files get installed
    [Parameter(Mandatory)] [string]$Exe,        # exe filename inside SourceDir
    [string]$Icon,                              # optional .ico for the shortcut
    [Parameter(Mandatory)] [string]$Out         # output .msi path
)
$ErrorActionPreference = 'Stop'

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    dotnet tool install --global wix | Out-Null
}

# Deterministic UpgradeCode per app so a newer MSI replaces the older install.
$md5 = [System.Security.Cryptography.MD5]::Create()
$upgrade = [Guid]::new($md5.ComputeHash([Text.Encoding]::UTF8.GetBytes("rustjay-msi:$Name")))

$components = (Get-ChildItem $SourceDir -File | ForEach-Object {
    "        <Component><File Source=`"$($_.FullName)`" /></Component>"
}) -join "`n"

$iconXml = ''
$shortcutIcon = ''
if ($Icon -and (Test-Path $Icon)) {
    $iconXml = "<Icon Id=`"AppIcon`" SourceFile=`"$((Resolve-Path $Icon).Path)`" />"
    $shortcutIcon = ' Icon="AppIcon"'
}

$wxs = @"
<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">
  <Package Name="$Name" Manufacturer="BlueJayLouche" Version="$Version"
           UpgradeCode="$upgrade" Scope="perMachine">
    <MajorUpgrade DowngradeErrorMessage="A newer version of $Name is already installed." />
    <MediaTemplate EmbedCab="yes" />
    $iconXml
    <StandardDirectory Id="ProgramFiles64Folder">
      <Directory Id="INSTALLFOLDER" Name="$Name">
$components
      </Directory>
    </StandardDirectory>
    <StandardDirectory Id="ProgramMenuFolder">
      <Component Id="StartMenuShortcut">
        <Shortcut Id="AppShortcut" Name="$Name" Target="[INSTALLFOLDER]$Exe"$shortcutIcon />
        <RegistryValue Root="HKCU" Key="Software\BlueJayLouche\$Name" Name="installed"
                       Type="integer" Value="1" KeyPath="yes" />
      </Component>
    </StandardDirectory>
  </Package>
</Wix>
"@

$wxsPath = Join-Path ([IO.Path]::GetTempPath()) "$Name.wxs"
Set-Content $wxsPath $wxs -Encoding utf8
wix build $wxsPath -arch x64 -o $Out
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
"MSI: $Out ($([math]::Round((Get-Item $Out).Length/1MB,1)) MB)"
