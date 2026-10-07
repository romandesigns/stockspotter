// Standalone browser code for the WebView. Mirrors the tested web series writer;
// no closure over React Native state or module imports can cross this boundary.
export const chartSeriesWriterScript = String.raw`
function createSeriesWriter(series) {
  var previous = [];
  var initialized = false;
  function equal(a, b) {
    var keys = Object.keys(a);
    return keys.length === Object.keys(b).length && keys.every(function (key) { return a[key] === b[key]; });
  }
  return function (data) {
    var tailOnly = previous.length > 0 && data.length >= previous.length;
    if (tailOnly) {
      for (var i = 0; i < previous.length - 1; i++) {
        if (!equal(previous[i], data[i])) { tailOnly = false; break; }
      }
      if (previous[previous.length - 1].time !== data[previous.length - 1].time) tailOnly = false;
    }
    if (tailOnly) {
      for (var j = previous.length - 1; j < data.length; j++) {
        if (!previous[j] || !equal(previous[j], data[j])) series.update(data[j]);
      }
    } else if (!initialized || data.length || previous.length) {
      series.setData(data);
    }
    previous = data;
    initialized = true;
  };
}
`;
