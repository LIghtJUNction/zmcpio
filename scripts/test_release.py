import unittest
from release import next_version, parse_version, precedence

class VersionTests(unittest.TestCase):
    def test_bumps(self):
        self.assertEqual(next_version('1.2.3'), '1.2.4')
        self.assertEqual(next_version('1.2.3', 'minor'), '1.3.0')
        self.assertEqual(next_version('1.2.3', 'major'), '2.0.0')
    def test_manual(self):
        self.assertEqual(next_version('0.1.0', manual='0.1.0'), '0.1.0')
        self.assertEqual(next_version('0.1.0', manual='0.2.0-rc.1+build.2'), '0.2.0-rc.1+build.2')
        with self.assertRaises(ValueError): next_version('1.0.0', manual='0.9.9')
    def test_precedence(self):
        versions = ['1.0.0-alpha', '1.0.0-alpha.1', '1.0.0-alpha.beta', '1.0.0-beta', '1.0.0-beta.2', '1.0.0-beta.11', '1.0.0-rc.1', '1.0.0']
        self.assertEqual(sorted(versions, key=precedence), versions)
    def test_rejection(self):
        for version in ['', '1.0', '01.0.0', '1.0.0-01', '1.0.0;', '1.0.0\n', '1.0.0+bad space', '18446744073709551616.0.0']:
            with self.subTest(version=version), self.assertRaises(ValueError): parse_version(version)
        with self.assertRaises(ValueError): next_version('1.0.0', 'arbitrary')
        with self.assertRaises(ValueError): next_version('0.0.18446744073709551615')

if __name__ == '__main__': unittest.main()
