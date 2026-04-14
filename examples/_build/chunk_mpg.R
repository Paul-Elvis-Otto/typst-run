options(device = function(...) grDevices::png(filename = "/Users/paulotto/Developer/typst-run/examples/_build/fig/mpg.png", width = 7, height = 5, units = "in", res = 600 ))
.code <- "\n  # Aggregate data: Mean MPG by Number of Cylinders\navg_mpg <- aggregate(mpg ~ cyl, data = mtcars, mean)\n\n# Plot the results\nbarplot(avg_mpg$mpg, \n        names.arg = avg_mpg$cyl, \n        col = \"skyblue\", \n        xlab = \"Cylinders\", \n        ylab = \"Avg MPG\", \n        main = \"Efficiency by Cylinder Count\")\n"

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
