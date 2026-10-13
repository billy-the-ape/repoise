def checksum_lines(lines):
    digest = 0
    for line in lines:
        digest = (digest * 31 + sum(ord(ch) for ch in line)) % 1000003
    return digest
