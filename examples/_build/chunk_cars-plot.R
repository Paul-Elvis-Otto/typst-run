options(device = function(...) grDevices::png(filename = "/Users/paulotto/Developer/typst-run/examples/_build/fig/cars-plot.png", width = 7, height = 5, units = "in", res = 600 ))
.code <- "\n  plot(cars)\n"

tryCatch({
  .exprs <- parse(text = .code)
  for (.expr in .exprs) {
    .value <- withVisible(eval(.expr, envir = .GlobalEnv))
    if (.value$visible) print(.value$value)
  }
  if (grDevices::dev.cur() != 1) grDevices::dev.off()
}, error = function(e) {
  if (grDevices::dev.cur() != 1) grDevices::dev.off()
  message("ERROR: ", conditionMessage(e))
  quit(status = 1)
})
