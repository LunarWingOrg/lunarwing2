import unittest

from prepare_reproducible_build import prepare


class ReproducibleBuildTests(unittest.TestCase):
    source = """const result = await Bun.build({
    entrypoints: ["./src/index.ts"],
    bytecode: true,
    splitting: true,
    compile: { outfile: "opencode" },
    define: { OPENCODE_VERSION: "2.0.17" },
  })
  await verifyArtifact(outdir)
"""

    def test_changes_only_compilation_profile(self):
        patched = prepare(self.source)
        self.assertEqual(
            patched,
            self.source.replace("bytecode: true", "bytecode: false").replace(
                "splitting: true", "splitting: false"
            ),
        )

    def test_rejects_missing_or_duplicate_upstream_options(self):
        for option in ("bytecode", "splitting"):
            setting = f"    {option}: true,"
            for changed in (
                self.source.replace(setting, ""),
                self.source + setting,
                self.source.replace(setting, f"    {option}: false,"),
            ):
                with self.subTest(option=option, source=changed):
                    with self.assertRaises(ValueError):
                        prepare(changed)

    def test_rejects_second_application(self):
        with self.assertRaises(ValueError):
            prepare(prepare(self.source))


if __name__ == "__main__":
    unittest.main()
