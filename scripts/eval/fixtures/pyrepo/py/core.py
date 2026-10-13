def parse_manifest(text):
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    sections = {}
    for line in lines:
        name, value = line.split("=", 1)
        sections[name.strip()] = value.strip()
    return sections


def unknown_sections(sections, known):
    return sorted(set(sections) - set(known))
