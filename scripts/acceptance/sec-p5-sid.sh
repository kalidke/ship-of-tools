#!/usr/bin/env bash
# Security premise P5 of sec-hello (git-bash on a Windows box, needs a UAC click): the account SID is the same
# in a normal and an elevated process. PASS: the elevated window shows the same SID as the first line. Never paste SIDs publicly.
/c/Windows/System32/whoami.exe //user
powershell.exe -NoProfile -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoExit','-Command','C:\\Windows\\System32\\whoami.exe /user'"
