from cctrail.output import print_table


def test_preserves_nested_indent_when_truncating(capsys):
    print_table(
        ["Project", "Sessions"],
        [[f"  {'project' * 20}", "1"]],
        max_first_column=60,
    )

    assert capsys.readouterr().out.splitlines()[1].startswith("  project")
