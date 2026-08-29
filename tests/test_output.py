from cctrail.output import print_table


def test_preserves_nested_indent_when_truncating(capsys):
    print_table(
        ["Project", "Sessions"],
        [[f"  {'project' * 20}", "1"]],
        max_first_column=60,
    )

    assert capsys.readouterr().out.splitlines()[1].startswith("  project")


def test_table_fits_terminal_width(capsys, monkeypatch):
    monkeypatch.setenv("COLUMNS", "72")
    print_table(
        ["Project", "Sessions", "Total tokens", "Last active", "Cost", "Time"],
        [
            [
                "  ~/Documents/Codex/2026-08-06/a-very-long-project-name",
                "12",
                "123.45M",
                "2026-08-29",
                "$123.45",
                "12h 34m",
            ]
        ],
        max_first_column=60,
    )

    assert max(map(len, capsys.readouterr().out.splitlines())) <= 72
