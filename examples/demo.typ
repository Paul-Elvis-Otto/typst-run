// Define "do-nothing" functions
#let codechunk(..args) = none
#let codeoutput(..args) = none

= Typst Run MVP Demo

This plot is defined above and inserted later.



#codechunk("r", "cars-plot")[
  plot(cars)
]

#codeoutput("cars-plot")

#codechunk("r", "mpg")[
  # Aggregate data: Mean MPG by Number of Cylinders
avg_mpg <- aggregate(mpg ~ cyl, data = mtcars, mean)

# Plot the results
barplot(avg_mpg$mpg, 
        names.arg = avg_mpg$cyl, 
        col = "skyblue", 
        xlab = "Cylinders", 
        ylab = "Avg MPG", 
        main = "Efficiency by Cylinder Count")
]

#codeoutput("mpg")


this is a code chunk
