#!/usr/bin/env python3
"""Reproduce the three guarded Kernel overlays from the pinned v4 dependency."""
from pathlib import Path
import re
import posixpath

root = Path(__file__).resolve().parents[1]
src = root / 'dependencies/kernel-4.0.0/src'
out = root / 'src/kernel/guarded'
for name in ['Kernel.sol', 'core/ModuleManager.sol', 'core/ExecutionManager.sol']:
    text = (src / name).read_text()
    def imports(m):
        path = m[1]
        if name == 'Kernel.sol' and path in ('./core/ModuleManager.sol', './core/ExecutionManager.sol'):
            return m[0]
        return '"@zerodev/kernel/' + posixpath.normpath(posixpath.join(posixpath.dirname(name), path)) + '"'
    text = re.sub(r'"(\.\.?/[^"\n]+)"', imports, text)
    if name == 'core/ModuleManager.sol':
        text = text.replace('    /// @dev Modifier that wraps', '    function _beforeModuleMutation() internal view virtual {}\n\n    /// @dev Modifier that wraps', 1)
        for method in ['_installModule', '_uninstallModule']:
            start = text.index('    function ' + method + '(')
            pos = text.index('{', start) + 1
            text = text[:pos] + '\n        _beforeModuleMutation();' + text[pos:]
    elif name == 'core/ExecutionManager.sol':
        text = text.replace('    /// @notice Executes calldata', '    function _beforeExternalDelegateCall() internal virtual {}\n\n    /// @notice Executes calldata', 1)
        start = text.index('    function _delegateCall(')
        pos = text.index('{', start) + 1
        text = text[:pos] + '\n        _beforeExternalDelegateCall();' + text[pos:]
    else:
        for signature in ['function setRoot(Install[]', 'function setRoot(ValidationId', 'function grantAccess(']:
            start = text.index(signature)
            pos = text.index('{', start) + 1
            text = text[:pos] + '\n        _beforeModuleMutation();' + text[pos:]
    dest = out / name
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(text)
