from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest

BINARY = Path(__file__).resolve().parents[1] / 'target/debug/reactor.exe'


class RuntimeSmoke(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='reactor-m5-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.config = self.root / 'config'
        self.data = self.root / 'data'
        self.environment = dict(os.environ, REACTOR_CONFIG_DIR=str(self.config), REACTOR_DATA_DIR=str(self.data))
        self.invoke('init', '--no-autostart')

    def invoke(self, *arguments, success=True):
        result = subprocess.run([str(BINARY), '--output', 'json', *arguments], env=self.environment,
                                capture_output=True, text=True, timeout=40)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            json.loads(result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def schema(self):
        with closing(sqlite3.connect(self.data / 'reactor.db')) as database:
            return database.execute('PRAGMA user_version').fetchone()[0]

    def test_initialization_is_isolated(self):
        self.assertEqual(self.schema(), 10)
        result = self.invoke('status')
        self.assertIn('database_available', result.stdout)
        self.assertFalse((self.config / 'service-registration.json').exists())

    def test_backup_restore_stays_in_maintenance(self):
        self.invoke('maintenance', 'enter', '--timeout-seconds', '1')
        backup = self.root / 'backup'
        self.invoke('backup', 'create', str(backup))
        manifest = json.loads((backup / 'manifest.json').read_text())
        self.assertEqual(manifest['schema'], 10)
        self.assertEqual({item['path'] for item in manifest['files']}, {'reactor.db', 'config.json'})
        self.invoke('backup', 'restore', str(backup), '--replace')
        self.assertEqual(self.schema(), 10)
        self.assertTrue((self.data / 'maintenance.json').exists())
        self.assertFalse((self.data / 'restore.json').exists())
        self.assertTrue(list(self.data.glob('pre-restore-*')))

    def test_corrupt_backup_does_not_replace_current_data(self):
        self.invoke('maintenance', 'enter', '--timeout-seconds', '1')
        backup = self.root / 'backup'
        self.invoke('backup', 'create', str(backup))
        original = (self.config / 'config.json').read_bytes()
        (backup / 'config.json').write_bytes(b'corrupt')
        self.invoke('backup', 'restore', str(backup), '--replace', success=False)
        self.assertEqual((self.config / 'config.json').read_bytes(), original)
        self.assertEqual(self.schema(), 10)
        self.assertFalse((self.data / 'restore.json').exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
