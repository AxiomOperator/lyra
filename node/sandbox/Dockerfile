# lyra's sandbox browser: Chromium driven by Playwright, one container per
# task (`lyra-node sandbox setup` builds it; lyra starts and removes them).
FROM mcr.microsoft.com/playwright/python:v1.49.1-noble
# The Python package, for every user (the image has the browsers).
RUN pip install --no-cache-dir --break-system-packages playwright==1.49.1
COPY driver.py /lyra/driver.py
USER pwuser
ENTRYPOINT ["python3", "-u", "/lyra/driver.py"]
