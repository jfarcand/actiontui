#!/bin/bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Install this repo's git hooks (run once after cloning).

set -e
ROOT="$(git rev-parse --show-toplevel)"
chmod +x "$ROOT"/.build/hooks/*
git -C "$ROOT" config core.hooksPath .build/hooks
echo "✅ hooks installed — core.hooksPath = .build/hooks"
echo "   pre-commit (SPDX) · commit-msg (≤2 lines, no AI attribution) · pre-push (fmt+clippy+test)"
