options(device = function(...) grDevices::png(filename = "/Users/paulotto/Developer/typst-run/examples/_build/fig/cars-plot2.png", width = 1000, height = 700))
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
