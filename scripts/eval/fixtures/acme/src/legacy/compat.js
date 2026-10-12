function legacyShimV2(items) {
  return items.map((item) => String(item));
}

module.exports = { legacyShimV2 };
