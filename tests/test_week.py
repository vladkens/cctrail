import calendar
import locale
import sys

import pytest

from cctrail.week import first_weekday, macos_first_weekday


def test_locale_controls_the_first_weekday():
    assert first_weekday("en_US.UTF-8") == calendar.SUNDAY
    assert first_weekday("en_IE.UTF-8") == calendar.MONDAY
    assert first_weekday("ar_EG.UTF-8") == calendar.SATURDAY
    assert first_weekday("dv_MV.UTF-8") == calendar.FRIDAY


def test_system_calendar_takes_priority_over_terminal_locale(monkeypatch):
    monkeypatch.setattr("cctrail.week.macos_first_weekday", lambda: calendar.MONDAY)
    monkeypatch.setenv("LANG", "en_US.UTF-8")
    monkeypatch.delenv("LC_ALL", raising=False)
    monkeypatch.delenv("LC_TIME", raising=False)

    assert first_weekday() == calendar.MONDAY


def test_terminal_locale_is_the_fallback(monkeypatch):
    monkeypatch.setattr("cctrail.week.macos_first_weekday", lambda: None)
    monkeypatch.setattr("cctrail.week.locale.getlocale", lambda category: (None, None))
    monkeypatch.setenv("LANG", "en_US.UTF-8")
    monkeypatch.delenv("LC_ALL", raising=False)
    monkeypatch.delenv("LC_TIME", raising=False)

    assert first_weekday() == calendar.SUNDAY


@pytest.mark.parametrize("platform", ["linux", "win32"])
def test_other_platforms_do_not_load_core_foundation(monkeypatch, platform):
    monkeypatch.setattr("cctrail.week.sys.platform", platform)

    def unexpected_load(path):
        raise AssertionError(f"loaded {path}")

    monkeypatch.setattr("cctrail.week.ctypes.CDLL", unexpected_load)

    assert macos_first_weekday() is None
    assert first_weekday("en_US.UTF-8") == calendar.SUNDAY


def test_broken_process_locale_falls_back_without_crashing(monkeypatch):
    monkeypatch.setattr("cctrail.week.macos_first_weekday", lambda: None)

    def broken_locale(category):
        raise locale.Error("broken locale")

    monkeypatch.setattr("cctrail.week.locale.getlocale", broken_locale)
    monkeypatch.setenv("LANG", "en_IE.UTF-8")
    monkeypatch.delenv("LC_ALL", raising=False)
    monkeypatch.delenv("LC_TIME", raising=False)

    assert first_weekday() == calendar.MONDAY


@pytest.mark.skipif(sys.platform != "darwin", reason="requires Core Foundation")
def test_macos_first_weekday_reads_the_current_calendar():
    assert macos_first_weekday() in range(7)
