import os
import re
import tomllib
from dataclasses import dataclass
from pathlib import Path


class ConfigError(ValueError):
    pass


def config_path() -> Path:
    override = os.environ.get("CCTRAIL_CONFIG")
    if override:
        return Path(override).expanduser()

    local = Path.cwd() / "cctrail.toml"
    if local.is_file():
        return local

    root = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")).expanduser()
    return root / "cctrail.toml"


def normalize_path(value: str) -> str:
    return os.path.normpath(os.path.expanduser(value)).replace(os.sep, "/")


def match_path(path: str, pattern: str) -> bool:
    expression = ""
    index = 0
    while index < len(pattern):
        if pattern[index : index + 2] == "**":
            if index + 2 < len(pattern) and pattern[index + 2] == "/":
                expression += "(?:.*/)?"
                index += 3
            else:
                expression += ".*"
                index += 2
        elif pattern[index] == "*":
            expression += "[^/]*"
            index += 1
        else:
            expression += re.escape(pattern[index])
            index += 1
    return re.fullmatch(expression, path) is not None


@dataclass(frozen=True)
class ProjectAlias:
    name: str
    paths: tuple[str, ...]


@dataclass(frozen=True)
class Config:
    aliases: tuple[ProjectAlias, ...] = ()

    @classmethod
    def load(cls, path: Path | None = None) -> "Config":
        path = config_path() if path is None else path
        try:
            with path.open("rb") as handle:
                value = tomllib.load(handle)
        except FileNotFoundError:
            return cls()
        except (OSError, tomllib.TOMLDecodeError) as exc:
            raise ConfigError(f"cannot read {path}: {exc}") from exc

        raw_aliases = value.get("aliases", {})
        if not isinstance(raw_aliases, dict):
            raise ConfigError("aliases must be a table")

        aliases = []
        exact_paths: set[str] = set()
        for raw_name, raw_paths in raw_aliases.items():
            name = str(raw_name).strip()
            if not name:
                raise ConfigError("project alias names cannot be empty")
            if (
                not isinstance(raw_paths, list)
                or not raw_paths
                or not all(isinstance(item, str) and item.strip() for item in raw_paths)
            ):
                raise ConfigError(f"project alias {name} needs at least one path")

            paths = tuple(normalize_path(item) for item in raw_paths)
            for member in paths:
                if "*" not in member:
                    if member in exact_paths:
                        raise ConfigError(f"project path appears in multiple aliases: {member}")
                    exact_paths.add(member)
            aliases.append(ProjectAlias(name, paths))
        return cls(tuple(aliases))

    def resolve_project(self, project: str) -> str:
        normalized = normalize_path(project)
        matches = [
            alias.name
            for alias in self.aliases
            if any(match_path(normalized, pattern) for pattern in alias.paths)
        ]
        if len(matches) > 1:
            raise ConfigError(f"project {project} matches multiple aliases: {', '.join(matches)}")
        return matches[0] if matches else project
