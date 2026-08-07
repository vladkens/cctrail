import calendar
import ctypes
import locale
import os
import sys

CORE_FOUNDATION = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
FIRST_WEEKDAY_REGIONS = {
    calendar.FRIDAY: "MV",
    calendar.SATURDAY: "AF BH DJ DZ EG IQ IR JO KW LY OM QA SD SY",
    calendar.SUNDAY: (
        "AG AS BD BR BS BT BW BZ CA CO DM DO ET GT GU HK HN ID IL IN IS JM JP KE KH KR "
        "LA MH MM MO MT MX MZ NI NP PA PE PH PK PR PT PY SA SG SV TH TT TW UM US VE VI "
        "WS YE ZA ZW"
    ),
}


def macos_first_weekday() -> int | None:
    if sys.platform != "darwin":
        return None

    try:
        core_foundation = ctypes.CDLL(CORE_FOUNDATION)
        copy_current = core_foundation.CFCalendarCopyCurrent
        copy_current.argtypes = []
        copy_current.restype = ctypes.c_void_p
        get_first_weekday = core_foundation.CFCalendarGetFirstWeekday
        get_first_weekday.argtypes = [ctypes.c_void_p]
        get_first_weekday.restype = ctypes.c_long
        release = core_foundation.CFRelease
        release.argtypes = [ctypes.c_void_p]
        release.restype = None

        calendar_ref = copy_current()
        if not calendar_ref:
            return None
        try:
            value = get_first_weekday(calendar_ref)
        finally:
            release(calendar_ref)
    except (AttributeError, OSError):
        return None

    return (value + 5) % 7 if 1 <= value <= 7 else None


def first_weekday(locale_name: str | None = None) -> int:
    if locale_name is None:
        system_value = macos_first_weekday()
        if system_value is not None:
            return system_value

        try:
            process_locale = locale.getlocale(locale.LC_TIME)[0]
        except (locale.Error, ValueError):
            process_locale = None
        locale_name = (
            process_locale
            or os.environ.get("LC_ALL")
            or os.environ.get("LC_TIME")
            or os.environ.get("LANG")
            or ""
        )

    name = locale_name.split(".", 1)[0].split("@", 1)[0]
    parts = name.replace("-", "_").split("_")
    region = next(
        (part.upper() for part in reversed(parts[1:]) if len(part) == 2 and part.isalpha()),
        None,
    )
    for weekday, regions in FIRST_WEEKDAY_REGIONS.items():
        if region in regions.split():
            return weekday
    return calendar.MONDAY
