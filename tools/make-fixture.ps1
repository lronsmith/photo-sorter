<#
.SYNOPSIS
    Creates a small fake Google Takeout folder for manually testing Photo Sorter.

.DESCRIPTION
    Generates coloured, labelled JPG/PNG images with Takeout-style JSON sidecars
    covering every sidecar lookup rule (plain .json, .supplemental-metadata.json,
    "(1)" numbered duplicates, "-edited" copies, names truncated to 51 chars),
    plus a file with no metadata, a byte-identical album copy, files that must
    be ignored, and a video if one can be found under C:\Windows.

    Each sidecar carries a different, clearly old date (2014-2019) so that after
    pressing Y you can confirm in Explorer that Created/Modified changed.

    Layout:
        <Path>\Takeout\...      choose this as the source folder
        <Path>\Keep\            choose this as the keep folder (created empty)

.PARAMETER Path
    Directory to create the fixture in. Created if missing.

.PARAMETER Force
    Delete an existing <Path>\Takeout and <Path>\Keep first. Only folders made
    by this script (they contain a .photo-sorter-fixture marker file) are ever
    deleted; any other existing folder stops the script with an error.

.PARAMETER VideoPath
    Use this video file instead of searching C:\Windows.

.PARAMETER NoVideo
    Skip the video search.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\make-fixture.ps1 -Path $env:TEMP\photo-sorter-fixture -Force
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [switch]$Force,
    [string]$VideoPath,
    [switch]$NoVideo
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = [System.IO.Path]::GetFullPath($Path)
$takeout = Join-Path $root 'Takeout'
$keep = Join-Path $root 'Keep'

# Every folder this script creates gets this marker. -Force only ever deletes
# a folder that has it, so it can't wipe a real Takeout export or keep folder.
$markerName = '.photo-sorter-fixture'

function Stop-WithError([string]$message) {
    $Host.UI.WriteErrorLine($message)
    exit 1
}

# Check both folders before deleting either.
$toDelete = @()
foreach ($dir in @($takeout, $keep)) {
    if (Test-Path -LiteralPath $dir) {
        if (-not $Force) {
            Stop-WithError "$dir already exists. Re-run with -Force to replace it (only works on a folder this script made)."
        }
        if (-not (Test-Path -LiteralPath (Join-Path $dir $markerName) -PathType Leaf)) {
            Stop-WithError "Refusing to delete $dir`: it has no $markerName marker, so it was not made by this script. Choose a different -Path, or move that folder away yourself."
        }
        $toDelete += $dir
    }
}
foreach ($dir in $toDelete) {
    Remove-Item -LiteralPath $dir -Recurse -Force
}

$photos = Join-Path $takeout 'Google Photos'
$y2019 = Join-Path $photos 'Photos from 2019'
$y2018 = Join-Path $photos 'Photos from 2018'
$album = Join-Path $photos 'Trip to Paris'
foreach ($dir in @($y2019, $y2018, $album, $keep)) {
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
}

$utf8NoBom = New-Object System.Text.UTF8Encoding $false
foreach ($dir in @($takeout, $keep)) {
    [System.IO.File]::WriteAllText((Join-Path $dir $markerName), "Created by tools\make-fixture.ps1. make-fixture.ps1 -Force may delete this folder.`r`n", $utf8NoBom)
}
$summary = New-Object System.Collections.Generic.List[object]

function Write-Text([string]$file, [string]$text) {
    # No BOM: serde_json rejects a leading BOM.
    [System.IO.File]::WriteAllText($file, $text, $utf8NoBom)
}

function Add-Row([string]$file, [string]$expect) {
    $rel = $file.Substring($takeout.Length + 1)
    $summary.Add([pscustomobject]@{ File = $rel; Expect = $expect })
}

function New-Image([string]$file, [string]$label, [System.Drawing.Color]$colour, [string]$caption) {
    $bmp = New-Object System.Drawing.Bitmap 1200, 800
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias
        $g.Clear($colour)
        $g.FillRectangle([System.Drawing.Brushes]::White, 60, 60, 1080, 680)
        $inner = New-Object System.Drawing.SolidBrush $colour
        $g.FillRectangle($inner, 90, 90, 1020, 620)
        $inner.Dispose()

        $format = New-Object System.Drawing.StringFormat
        $format.Alignment = [System.Drawing.StringAlignment]::Center
        $format.LineAlignment = [System.Drawing.StringAlignment]::Center
        $big = New-Object System.Drawing.Font 'Segoe UI', 110, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
        $small = New-Object System.Drawing.Font 'Segoe UI', 40, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
        $g.DrawString($label, $big, [System.Drawing.Brushes]::White, (New-Object System.Drawing.RectangleF 90, 120, 1020, 400), $format)
        $g.DrawString($caption, $small, [System.Drawing.Brushes]::White, (New-Object System.Drawing.RectangleF 90, 520, 1020, 160), $format)
        $big.Dispose(); $small.Dispose(); $format.Dispose()

        $ext = [System.IO.Path]::GetExtension($file).ToLowerInvariant()
        if ($ext -eq '.png') {
            $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
        } else {
            $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Jpeg)
        }
    } finally {
        $g.Dispose()
        $bmp.Dispose()
    }
}

function Get-TakenDate([int]$year, [int]$month, [int]$day, [int]$hour, [int]$minute) {
    return New-Object System.DateTimeOffset $year, $month, $day, $hour, $minute, 0, ([TimeSpan]::Zero)
}

function New-Sidecar([string]$file, [string]$title, [System.DateTimeOffset]$taken) {
    $seconds = $taken.ToUnixTimeSeconds()
    $formatted = $taken.UtcDateTime.ToString('d MMM yyyy, HH:mm:ss', [System.Globalization.CultureInfo]::InvariantCulture) + ' UTC'
    $json = @"
{
  "title": "$title",
  "description": "",
  "creationTime": {
    "timestamp": "$seconds",
    "formatted": "$formatted"
  },
  "photoTakenTime": {
    "timestamp": "$seconds",
    "formatted": "$formatted"
  }
}
"@
    Write-Text $file $json
}

function Format-Taken([System.DateTimeOffset]$taken) {
    # Matches the app's display (local time).
    return 'Taken ' + $taken.LocalDateTime.ToString('d MMM yyyy HH:mm', [System.Globalization.CultureInfo]::InvariantCulture)
}

# 1. Plain <name>.json sidecar.
$d = Get-TakenDate 2019 7 14 10 30
$f = Join-Path $y2019 'IMG_0001.jpg'
New-Image $f 'IMG_0001' ([System.Drawing.Color]::SteelBlue) 'plain .json sidecar'
New-Sidecar (Join-Path $y2019 'IMG_0001.jpg.json') 'IMG_0001.jpg' $d
Add-Row $f "$(Format-Taken $d)  (IMG_0001.jpg.json)"

# 2. <name>.supplemental-metadata.json sidecar.
$d = Get-TakenDate 2018 3 2 16 45
$f = Join-Path $y2019 'IMG_0002.png'
New-Image $f 'IMG_0002' ([System.Drawing.Color]::SeaGreen) 'supplemental-metadata sidecar'
New-Sidecar (Join-Path $y2019 'IMG_0002.png.supplemental-metadata.json') 'IMG_0002.png' $d
Add-Row $f "$(Format-Taken $d)  (IMG_0002.png.supplemental-metadata.json)"

# 3. Original + "(1)" numbered duplicate (different image, so a different size).
$d = Get-TakenDate 2017 12 24 18 0
$f = Join-Path $y2019 'IMG_0003.jpg'
New-Image $f 'IMG_0003' ([System.Drawing.Color]::DarkOrange) 'original of the (1) duplicate'
New-Sidecar (Join-Path $y2019 'IMG_0003.jpg.json') 'IMG_0003.jpg' $d
Add-Row $f "$(Format-Taken $d)  (IMG_0003.jpg.json)"

$d = Get-TakenDate 2016 12 25 9 15
$f = Join-Path $y2019 'IMG_0003(1).jpg'
New-Image $f 'IMG_0003(1)' ([System.Drawing.Color]::Firebrick) 'numbered duplicate: IMG_0003.jpg(1).json'
New-Sidecar (Join-Path $y2019 'IMG_0003.jpg(1).json') 'IMG_0003.jpg' $d
Add-Row $f "$(Format-Taken $d)  (IMG_0003.jpg(1).json)"

# 4. Original + "-edited" copy with no sidecar of its own.
$d = Get-TakenDate 2015 5 9 13 20
$f = Join-Path $y2019 'IMG_0004.jpg'
New-Image $f 'IMG_0004' ([System.Drawing.Color]::MediumPurple) 'original of the -edited copy'
New-Sidecar (Join-Path $y2019 'IMG_0004.jpg.json') 'IMG_0004.jpg' $d
Add-Row $f "$(Format-Taken $d)  (IMG_0004.jpg.json)"

$f = Join-Path $y2019 'IMG_0004-edited.jpg'
New-Image $f 'IMG_0004-edited' ([System.Drawing.Color]::Indigo) 'edited copy: date from IMG_0004.jpg.json'
Add-Row $f "$(Format-Taken $d)  (from the original's IMG_0004.jpg.json)"

# 5. No metadata at all (System.Drawing writes no EXIF date).
$f = Join-Path $y2019 'Screenshot_no_metadata.png'
New-Image $f 'NO METADATA' ([System.Drawing.Color]::DimGray) 'no sidecar, no EXIF: Taken: unknown'
Add-Row $f 'Taken: unknown  (file times left unchanged)'

# 6. Long name: plain sidecar truncated to 51 chars including .json.
$longName = 'Family_picnic_by_the_lake_with_everyone_summer_2019.jpg'
if ($longName.Length -lt 47) { throw 'long name must be at least 47 chars' }
$truncated = $longName.Substring(0, 46) + '.json'
$d = Get-TakenDate 2014 8 3 11 5
$f = Join-Path $y2019 $longName
New-Image $f 'LONG NAME' ([System.Drawing.Color]::Teal) "sidecar: $truncated"
New-Sidecar (Join-Path $y2019 $truncated) $longName $d
Add-Row $f "$(Format-Taken $d)  ($truncated, $($truncated.Length) chars)"

# 7. Medium name: supplemental-metadata sidecar truncated to 51 chars.
$pxlName = 'PXL_20190704_183015123.MP.jpg'
$supp = ($pxlName + '.supplemental-metadata').Substring(0, 46) + '.json'
$d = Get-TakenDate 2018 7 4 18 30
$f = Join-Path $y2018 $pxlName
New-Image $f 'PXL' ([System.Drawing.Color]::Goldenrod) "sidecar: $supp"
New-Sidecar (Join-Path $y2018 $supp) $pxlName $d
Add-Row $f "$(Format-Taken $d)  ($supp, $($supp.Length) chars)"

# 8. Byte-identical copy of IMG_0001.jpg in an album: shown once (same name + size).
$f = Join-Path $album 'IMG_0001.jpg'
Copy-Item -LiteralPath (Join-Path $y2019 'IMG_0001.jpg') -Destination $f
Copy-Item -LiteralPath (Join-Path $y2019 'IMG_0001.jpg.json') -Destination (Join-Path $album 'IMG_0001.jpg.json')
Add-Row $f 'NOT SHOWN: duplicate of Photos from 2019/IMG_0001.jpg (left in place)'

# 9. Files the scan must ignore.
Write-Text (Join-Path $album 'metadata.json') '{"title": "Trip to Paris", "description": ""}'
Write-Text (Join-Path $takeout 'archive_browser.html') '<html><body>Takeout archive browser (ignored)</body></html>'
Write-Text (Join-Path $y2019 'notes.txt') 'Not a photo: must be ignored by Photo Sorter.'
Add-Row (Join-Path $takeout 'archive_browser.html') 'IGNORED (not media)'
Add-Row (Join-Path $y2019 'notes.txt') 'IGNORED (not media)'
Add-Row (Join-Path $album 'metadata.json') 'IGNORED (not media)'

# 10. A video, if one can be found.
$video = $null
if (-not $NoVideo) {
    if ($VideoPath) {
        $video = (Resolve-Path -LiteralPath $VideoPath).Path
    } else {
        Write-Host 'Looking for a sample .mp4/.wmv under C:\Windows (up to 60 s)...'
        $job = Start-Job -ScriptBlock {
            $quick = @('C:\Windows\Media', 'C:\Windows\Web', 'C:\Windows\SystemResources',
                       'C:\Windows\System32\DriverStore\FileRepository')
            foreach ($dir in ($quick + 'C:\Windows')) {
                $hit = Get-ChildItem -Path $dir -Recurse -Include *.mp4, *.wmv -File -ErrorAction SilentlyContinue |
                    Select-Object -First 1 -ExpandProperty FullName
                if ($hit) { return $hit }
            }
        }
        if (Wait-Job $job -Timeout 60) {
            $video = Receive-Job $job -ErrorAction SilentlyContinue | Select-Object -First 1
        }
        Remove-Job $job -Force
    }
}
if ($video) {
    $ext = [System.IO.Path]::GetExtension($video).ToLowerInvariant()
    $videoName = 'VID_20160611_120000' + $ext
    $d = Get-TakenDate 2016 6 11 12 0
    $f = Join-Path $y2019 $videoName
    Copy-Item -LiteralPath $video -Destination $f
    New-Sidecar (Join-Path $y2019 ($videoName + '.json')) $videoName $d
    Add-Row $f "$(Format-Taken $d)  (video, copied from $video)"
} else {
    Write-Host 'No video found (or -NoVideo given): skipping the video item.' -ForegroundColor Yellow
}

# Sanity check: the (1) duplicate must not collapse into its original.
$a = (Get-Item -LiteralPath (Join-Path $y2019 'IMG_0003.jpg')).Length
$b = (Get-Item -LiteralPath (Join-Path $y2019 'IMG_0003(1).jpg')).Length
if ($a -eq $b) { Write-Warning 'IMG_0003.jpg and IMG_0003(1).jpg have the same size (unexpected).' }

Write-Host ''
Write-Host "Fake Takeout created under $root" -ForegroundColor Green
Write-Host "  Source folder: $takeout"
Write-Host "  Keep folder:   $keep (empty)"
Write-Host ''
$summary | Sort-Object File | Format-Table -AutoSize -Wrap | Out-String -Width 220 | Write-Host
Write-Host 'Not included: a HEIC file (cannot be generated here). Copy one in by hand to test the HEIC preview / Space fallback.'
Write-Host 'Decisions are saved in Takeout\.photo-sorter-skipped.txt and Takeout\.photo-sorter-kept.txt; re-run with -Force to start over.'
