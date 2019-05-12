"""A setuptools based setup module.
See:
https://packaging.python.org/en/latest/distributing.html
https://github.com/pypa/sampleproject
"""

from setuptools import setup, find_packages
from os import path

here = path.abspath(path.dirname(__file__))
with open(path.join(here, "README.rst"), encoding="utf-8") as f:
    long_description = f.read()

extras_require = {
    "dev": ["check-manifest", "colorama", "pygments", "twine", "wheel", "aiorun"],
    "test": ["pytest", "pytest-cov", "portpicker", "pytest-benchmark"],
    "doc": ["sphinx"],
}
extras_require["all"] = list({pkg for k, v in extras_require.items() for pkg in v})

setup(
    name="aiomsg",
    version=open("VERSION").readline().strip(),
    description="Socket-based abstraction for messaging patterns",
    long_description=long_description,
    long_description_content_type="text/x-rst",
    url="https://github.com/cjrh/aiomsg",
    author="Caleb Hattingh",
    author_email="caleb.hattingh@gmail.com",
    classifiers=[
        "Development Status :: 3 - Alpha",
        "Intended Audience :: Developers",
        "Topic :: Software Development :: Build Tools",
        "License :: OSI Approved :: Apache Software License",
        "Programming Language :: Python :: 3.6",
        "Programming Language :: Python :: 3.7",
        "Programming Language :: Python :: 3.8",
        "Framework :: AsyncIO",
        "Topic :: Communications",
    ],
    keywords="asyncio socket network",
    packages=find_packages(exclude=["contrib", "docs", "tests"]),
    extras_require=extras_require,
)
