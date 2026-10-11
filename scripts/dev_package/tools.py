"""Resolve already installed tools without invoking mutating manager shims."""
import json
import os
from pathlib import Path
import platform
import tomllib


class ToolUnavailable(ValueError): pass


def rustup_binary(path, name, root):
    path=Path(path)
    sibling=path.with_name('rustup')
    if path.resolve().name!='rustup' and not (sibling.exists() and os.path.samefile(path,sibling)):
        return str(path)
    rustup_home=Path(os.environ.get('RUSTUP_HOME',str(Path.home()/'.rustup')))
    # Reading settings is safe; asking rustup for them initializes missing homes.
    settings_path=rustup_home/'settings.toml'
    settings=tomllib.loads(settings_path.read_text()) if settings_path.is_file() else {}
    if not isinstance(settings,dict) or not isinstance(settings.get('overrides',{}),dict):
        raise ToolUnavailable('Unrecognized Rustup settings structure')
    channel=os.environ.get('RUSTUP_TOOLCHAIN')
    root=Path(root).resolve()
    if not channel:
        overrides=settings.get('overrides',{})
        for parent in (root,*root.parents):
            if str(parent) in overrides:
                channel=overrides[str(parent)]; break
    if not channel:
        for parent in (root,*root.parents):
            declaration=parent/'rust-toolchain.toml'
            legacy=parent/'rust-toolchain'
            if declaration.is_file():
                channel=tomllib.loads(declaration.read_text())['toolchain']['channel']; break
            if legacy.is_file():
                text=legacy.read_text().strip()
                channel=tomllib.loads(text)['toolchain']['channel'] if text.startswith('[') else text
                break
    channel=channel or settings.get('default_toolchain')
    if not channel or not isinstance(channel,str): raise ToolUnavailable('No installed Rust toolchain is selected')
    if Path(channel).is_absolute(): target=Path(channel)/'bin'/name
    else:
        if '/' in channel or '\\' in channel or channel in ('.','..'): raise ToolUnavailable('Invalid selected Rust toolchain')
        target=rustup_home/'toolchains'/channel/'bin'/name
        if not target.is_file():
            host={('Darwin','arm64'):'aarch64-apple-darwin',('Linux','x86_64'):'x86_64-unknown-linux-gnu',('Linux','aarch64'):'aarch64-unknown-linux-gnu'}.get((platform.system(),platform.machine()))
            if host: target=rustup_home/'toolchains'/f'{channel}-{host}'/'bin'/name
    if not target.is_file() or not os.access(target,os.X_OK):
        raise ToolUnavailable('Selected Rust toolchain is not installed; select an existing supported toolchain')
    return str(target)


def pnpm_binary(path, root):
    path=Path(path)
    resolved=path.resolve()
    shim='corepack' in resolved.parts
    if not shim and resolved.is_file():
        with resolved.open('rb') as stream: shim=b'corepack.cjs' in stream.read(4096)
    if not shim: return str(path)
    package=json.loads((Path(root)/'apps/desktop/package.json').read_text())
    version=package['packageManager'].split('@',1)[1]
    base=Path(os.environ.get('COREPACK_HOME', str(Path(os.environ.get('XDG_CACHE_HOME',os.environ.get('LOCALAPPDATA',str(Path.home()/'.cache'))))/'node/corepack')))
    # Corepack's current v1 cache layout. Unsupported layouts fail closed.
    install=base/'v1/pnpm'/version
    receipt=install/'.corepack'
    manifest=install/'package.json'
    if not receipt.is_file() or not manifest.is_file():
        raise ToolUnavailable('Required pnpm is not already installed in the Corepack cache; select an existing pnpm executable')
    metadata=json.loads(manifest.read_text())
    marker=json.loads(receipt.read_text())
    if not isinstance(metadata,dict) or not isinstance(metadata.get('bin'),dict):
        raise ToolUnavailable('Unrecognized installed pnpm metadata structure')
    if metadata.get('name')!='pnpm' or metadata.get('version')!=version or not isinstance(marker,dict):
        raise ToolUnavailable('Unrecognized installed pnpm metadata')
    binary=metadata.get('bin',{}).get('pnpm')
    if not isinstance(binary,str) or Path(binary).is_absolute() or '..' in Path(binary).parts:
        raise ToolUnavailable('Unrecognized installed pnpm binary path')
    target=install/binary
    if not target.is_file() or not os.access(target,os.X_OK):
        raise ToolUnavailable('Installed pnpm executable is missing')
    return str(target)


def installed_binary(path,name,root):
    if name in ('cargo','rustc'): return rustup_binary(path,name,root)
    if name=='pnpm': return pnpm_binary(path,root)
    return path
