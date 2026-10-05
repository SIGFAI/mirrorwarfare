@echo off
rem Mirrorwarfare: local FFA on a Catalyst arena with bots. Options: see Mirrorwarfare.ps1
rem   Mirrorwarfare.cmd [-Arena mec_anchor_1] [-Bots 7] [-Spawn] [-Menu] [-List] [-NoSound]
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Mirrorwarfare.ps1" %*
