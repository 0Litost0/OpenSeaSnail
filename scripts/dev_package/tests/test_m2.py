import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from scripts.dev_package import native,network,recovery,state
from scripts.dev_package.errors import Failure


class NetworkTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.root=Path(self.temp.name).resolve(); self.cache=self.root/'cache'
        self.payload=b'pinned'; self.row=dict(file_name='input.bin',url='https://example.invalid/input',size_bytes=6,sha256=hashlib.sha256(self.payload).hexdigest())
    def tearDown(self): self.temp.cleanup()
    def curl(self,code=0,payload=None,http='200'):
        def run(command,**kwargs):
            path=Path(command[command.index('--output')+1]); path.write_bytes(self.payload if payload is None else payload)
            return subprocess.CompletedProcess(command,code,stdout=http)
        return run
    def test_retry_is_bounded_and_partial_not_published(self):
        with patch.object(network.subprocess,'run',side_effect=self.curl(28,b'partial')) as mocked:
            with self.assertRaises(Failure) as caught: network.download(self.root,self.cache,self.row,'run',sleeper=lambda _:None)
        self.assertEqual(mocked.call_count,3); self.assertEqual(caught.exception.exit_code,4)
        self.assertFalse((self.cache/'input.bin').exists()); self.assertFalse(list((self.cache/'.dev-package-downloads').iterdir()))
    def test_bad_hash_fails_without_retry_or_overwrite(self):
        with patch.object(network.subprocess,'run',side_effect=self.curl(payload=b'broken')) as mocked:
            with self.assertRaises(Failure) as caught: network.download(self.root,self.cache,self.row,'run',sleeper=lambda _:None)
        self.assertEqual(mocked.call_count,1); self.assertEqual(caught.exception.exit_code,5)
        self.assertFalse((self.cache/'input.bin').exists())
    def test_valid_file_is_published_and_reused(self):
        with patch.object(network.subprocess,'run',side_effect=self.curl()) as mocked:
            path=network.download(self.root,self.cache,self.row,'run')
            network.download(self.root,self.cache,self.row,'next-run')
        self.assertEqual(path.read_bytes(),self.payload); self.assertEqual(mocked.call_count,1)
    def test_http_transient_retries_but_404_does_not(self):
        for http,attempts in [('503',3),('429',3),('404',1)]:
            with self.subTest(http=http),patch.object(network.subprocess,'run',side_effect=self.curl(22,http=http)) as mocked:
                with self.assertRaises(Failure) as caught: network.download(self.root,self.cache,self.row,'run',sleeper=lambda _:None)
            self.assertEqual(mocked.call_count,attempts); self.assertEqual(caught.exception.exit_code,4)
    def test_credentials_and_non_https_are_rejected(self):
        for url in ['http://example.invalid','https://user:secret@example.invalid/input']:
            with patch.object(network.subprocess,'run',side_effect=AssertionError('must not download')):
                with self.assertRaises(Failure): network.download(self.root,self.cache,dict(self.row,url=url),'run')
    def test_existing_invalid_input_is_not_replaced(self):
        self.cache.mkdir(); (self.cache/'input.bin').write_bytes(b'user-modified')
        with patch.object(network.subprocess,'run',side_effect=AssertionError('must not download')):
            with self.assertRaises(Failure): network.download(self.root,self.cache,self.row,'run')
        self.assertEqual((self.cache/'input.bin').read_bytes(),b'user-modified')


class OwnershipTests(unittest.TestCase):
    def test_unknown_directory_is_preserved(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); (root/'workspace').mkdir(); (root/'workspace/user').write_text('preserve')
            with self.assertRaises(Failure): recovery.owned_directory(root,'workspace','run','native',reuse=True)
            self.assertEqual((root/'workspace/user').read_text(),'preserve')
    def test_symlink_escape_cannot_be_owned(self):
        with tempfile.TemporaryDirectory() as folder,tempfile.TemporaryDirectory() as outside:
            root=Path(folder).resolve(); (root/'workspace').symlink_to(outside)
            with self.assertRaises(ValueError): recovery.owned_directory(root,'workspace/build','run','native')
            self.assertFalse(list(Path(outside).iterdir()))
    def test_receipt_detects_file_and_mode_tampering(self):
        with tempfile.TemporaryDirectory() as folder:
            base=Path(folder).resolve(); artifact=base/'artifact'; artifact.mkdir(); (artifact/'bin').write_bytes(b'validated')
            receipt=base/'receipt.json'; identity={'pinned':'identity'}
            native.create_receipt(receipt,identity,artifact,['synthetic validation'])
            self.assertTrue(native.reusable(receipt,identity,artifact))
            (artifact/'bin').chmod(0o755)
            with self.assertRaises(Failure): native.reusable(receipt,identity,artifact)
            (artifact/'bin').chmod(0o644); (artifact/'bin').write_bytes(b'tampered')
            with self.assertRaises(Failure): native.reusable(receipt,identity,artifact)
    def test_symlinks_are_relative_and_stay_in_artifact(self):
        with tempfile.TemporaryDirectory() as folder:
            artifact=Path(folder).resolve(); (artifact/'link').symlink_to('/etc/passwd')
            with self.assertRaises(Failure): native.inventory(artifact)


if __name__=='__main__': unittest.main()
