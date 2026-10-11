import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts.dev_package import native,publication,recovery,state,temporary
from scripts.dev_package.errors import Failure


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.root=Path(self.temp.name).resolve()
        self.base=recovery.owned_directory(self.root,'native','run','native-build')
        self.identity={'input':'pinned'}
    def tearDown(self): self.temp.cleanup()
    def staged(self):
        stage=publication.begin(self.root,self.base,'run'); stage.mkdir(); (stage/'bin').write_bytes(b'valid')
        return stage
    def test_publish_and_reuse(self):
        stage=self.staged(); publication.publish(self.root,self.base,stage,self.identity,['validated'])
        self.assertTrue(native.reusable(self.base/'receipt.json',self.identity,self.base/'artifact'))
        self.assertFalse(list(self.base.glob('staging-*')))
    def test_after_rename_interruption_recovers_receipt(self):
        stage=self.staged()
        original=publication.os.replace
        def crash(source,target):
            if Path(source).name=='.pending-receipt.json': raise OSError('injected after rename')
            return original(source,target)
        with patch.object(publication.os,'replace',side_effect=crash):
            with self.assertRaises(OSError): publication.publish(self.root,self.base,stage,self.identity,['validated'])
        self.assertTrue((self.base/'artifact').exists()); self.assertFalse((self.base/'receipt.json').exists())
        publication.recover(self.root,self.base,self.identity)
        self.assertTrue(native.reusable(self.base/'receipt.json',self.identity,self.base/'artifact'))
    def test_unpublished_stage_removed_without_publishing(self):
        stage=self.staged(); publication.recover(self.root,self.base,self.identity)
        self.assertFalse(stage.exists()); self.assertFalse((self.base/'artifact').exists())
    def test_unrecorded_stage_preserved(self):
        stage=self.base/'.artifact-unknown'; stage.mkdir(); (stage/'user').write_text('preserve')
        with self.assertRaises(Failure): publication.recover(self.root,self.base,self.identity)
        self.assertEqual((stage/'user').read_text(),'preserve')
    def test_changed_final_not_recovered(self):
        stage=self.staged()
        pending=self.base/'.pending-receipt.json'
        state.atomic_json(pending,dict(schema_version=1,identity=self.identity,artifact=str(self.base/'artifact'),inventory=native.inventory(stage),complete=True))
        stage.rename(self.base/'artifact'); (self.base/'artifact/bin').write_bytes(b'changed')
        with self.assertRaises(Failure): publication.recover(self.root,self.base,self.identity)
        self.assertTrue(pending.exists()); self.assertFalse((self.base/'receipt.json').exists())

    def test_only_owned_download_leaf_is_recovered(self):
        leaf=recovery.owned_directory(self.root,'downloads/known','old-run','download')
        (leaf/'partial').write_bytes(b'partial')
        self.assertEqual(temporary.recover_bucket(self.root,'downloads','download'),[str(leaf)])
        unknown=self.root/'downloads/user'; unknown.mkdir(); (unknown/'input').write_text('preserve')
        with self.assertRaises(Failure): temporary.recover_bucket(self.root,'downloads','download')
        self.assertEqual((unknown/'input').read_text(),'preserve')


if __name__=='__main__': unittest.main()
