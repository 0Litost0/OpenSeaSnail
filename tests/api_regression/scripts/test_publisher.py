"""Publication boundary checks; these do not count as business case execution."""
import base64, io, json, tempfile, unittest, zipfile
from pathlib import Path
from publish_report import Scanner, UnsafeReport, publish

class PublisherTests(unittest.TestCase):
    def archive(self,name,body):
        output=io.BytesIO()
        with zipfile.ZipFile(output,'w') as archive:archive.writestr(name,body)
        return output.getvalue()
    def test_embedded_zip_canary(self):
        data=self.archive('context.txt',b'API_TEST_ZIP_CANARY')
        with self.assertRaises(UnsafeReport):Scanner([]).scan('index.html',b'data:application/zip;base64,'+base64.b64encode(data))
    def test_malformed_archive_and_json(self):
        for label,data in [('bad.zip',b'broken'),('bad.json',b'{')]:
            with self.assertRaises(UnsafeReport):Scanner([]).scan(label,data)
    def test_archive_traversal(self):
        with self.assertRaises(UnsafeReport):Scanner([]).scan('bad.zip',self.archive('../file.txt',b'safe'))
    def test_nested_encoded_attachment(self):
        body=json.dumps({'body':base64.b64encode(b'local-fixture-sensitive-value').decode(),'contentType':'text/plain'}).encode()
        with self.assertRaises(UnsafeReport):Scanner(['local-fixture-sensitive-value']).scan('report.json',body)
    def test_limits(self):
        scanner=Scanner([]);scanner.entries=10000
        with self.assertRaises(UnsafeReport):scanner.scan('text.txt',b'safe')
        with self.assertRaises(UnsafeReport):Scanner([]).scan('text.txt',b'safe',13)
    def test_raw_symlink_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);(root/'real').mkdir();(root/'raw').symlink_to(root/'real')
            with self.assertRaises(UnsafeReport):publish(root/'raw',root/'published')

if __name__=='__main__':unittest.main()
